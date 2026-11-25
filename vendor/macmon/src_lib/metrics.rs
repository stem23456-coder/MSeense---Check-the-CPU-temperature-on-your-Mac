//! Metrics model and hardware sampler.

use core_foundation::dictionary::CFDictionaryRef;

use serde::Serialize;
use std::{collections::HashMap, time::Duration};

use crate::shared::{ioreport_channels_filter, zero_div};
use crate::sources::{
  IOHIDSensors, IOReport, SMC, SocInfo, cfio_get_residencies, cfio_watts, get_soc_info, libc_ram,
  libc_swap, sysctl_str,
};

type WithError<T> = Result<T, Box<dyn std::error::Error>>;
type CpuCoreKey = String;
type FreqMetrics = (u32, f32, f32);

// const CPU_FREQ_DICE_SUBG: &str = "CPU Complex Performance States";
const CPU_FREQ_CORE_SUBG: &str = "CPU Core Performance States";
const GPU_FREQ_DICE_SUBG: &str = "GPU Performance States";

// MARK: Structs

/// Average hardware temperatures.
#[derive(Debug, Default, Serialize)]
pub struct TempMetrics {
  /// Average CPU temperature in Celsius.
  pub cpu_temp_avg: f32,
  /// Average GPU temperature in Celsius.
  pub gpu_temp_avg: f32,
}

/// Memory and swap usage.
#[derive(Debug, Default, Serialize)]
pub struct MemMetrics {
  /// Total physical memory in bytes.
  pub ram_total: u64,
  /// Used physical memory in bytes.
  pub ram_usage: u64,
  /// Total configured swap in bytes.
  pub swap_total: u64,
  /// Used swap in bytes.
  pub swap_usage: u64,
}

/// Fan speed metrics.
#[derive(Debug, Default, Serialize)]
pub struct FanMetric {
  /// Stable fan name derived from the fan order, e.g. `fan0`.
  pub name: String,
  /// Current fan speed in revolutions per minute.
  pub rpm: u32,
  /// Maximum fan speed in revolutions per minute, when reported by SMC.
  pub max_rpm: Option<u32>,
}

/// Metrics for one CPU core.
#[derive(Debug, Default, Serialize)]
pub struct CpuCoreMetrics {
  /// Die index reported by the IOReport channel.
  pub die_id: usize,
  /// Core index within the die.
  pub core_id: usize,
  /// Average frequency in MHz while the core was active.
  pub freq_mhz: u32,
  /// Active residency weighted by operating frequency relative to the core maximum.
  pub scaled_ratio: f32,
  /// Fraction of the sampling interval spent in active frequency states.
  pub active_ratio: f32,
}

// Missing reports differ from real idle zeroes. Prefer a current CLPC reading;
// never add the same component from both CLPC and Energy Model.
#[derive(Default)]
struct PowerSources {
  clpc: Option<f32>,
  legacy: Option<f32>,
}

fn add_power(total: &mut Option<f32>, value: f32) {
  if value.is_finite() && value >= 0.0 {
    *total = Some(total.unwrap_or(0.0) + value);
  }
}

impl PowerSources {
  fn reading(self, allow_legacy: bool) -> Option<f32> {
    self.clpc.or(if allow_legacy { self.legacy } else { None })
  }
}

struct SmcSensors {
  smc: SMC,
  cpu_keys: Vec<String>,
  gpu_keys: Vec<String>,
  fan_keys: Vec<String>,
}

/// A complete metrics snapshot returned by [`Sampler`].
///
/// Scaled ratios weight active-state residency by operating frequency relative
/// to the hardware maximum. Active ratios count all active-state residency
/// equally, regardless of frequency. Both are in the `0.0..=1.0` range.
/// CPU cluster frequencies average the cores with measured active residency.
/// A frequency of zero means no active frequency was measured. Unavailable
/// residency ratios are NaN, including incomplete CPU aggregates.
/// Per-core and GPU frequencies are averaged over active residency.
/// Power values are reported in Watts.
///
/// This struct may gain new metrics in future releases. When constructing it
/// manually, for example in tests, use struct update syntax:
///
/// ```
/// # use macmon::Metrics;
/// let metrics = Metrics { cpu_power: Some(1.0), ..Default::default() };
/// ```
#[derive(Debug, Default, Serialize)]
pub struct Metrics {
  /// Temperature metrics.
  pub temp: TempMetrics,
  /// Memory and swap metrics.
  pub memory: MemMetrics,
  /// Fan metrics ordered by stable SMC fan key order.
  pub fans: Vec<FanMetric>,
  /// Combined frequency-weighted active residency across all CPU cores.
  pub cpu_scaled_ratio: f32,
  /// Combined fraction of the sampling interval spent in active CPU frequency states.
  pub cpu_active_ratio: f32,
  /// Efficiency-cluster frequency in MHz.
  pub ecpu_freq_mhz: u32,
  /// Mean frequency-weighted active residency across efficiency cores.
  pub ecpu_scaled_ratio: f32,
  /// Mean fraction of the sampling interval spent in active efficiency-core frequency states.
  pub ecpu_active_ratio: f32,
  /// Performance-cluster frequency in MHz.
  pub pcpu_freq_mhz: u32,
  /// Mean frequency-weighted active residency across performance cores.
  pub pcpu_scaled_ratio: f32,
  /// Mean fraction of the sampling interval spent in active performance-core frequency states.
  pub pcpu_active_ratio: f32,
  /// Metrics for efficiency cores, ordered by die and core index.
  pub ecpu_cores: Vec<CpuCoreMetrics>,
  /// Metrics for performance cores, ordered by die and core index.
  pub pcpu_cores: Vec<CpuCoreMetrics>,
  /// Middle-tier Performance cores on three-tier chips (M6).
  pub mcpu_cores: Vec<CpuCoreMetrics>,
  /// Middle-tier frequency in MHz.
  pub mcpu_freq_mhz: u32,
  /// Middle-tier frequency-weighted active residency.
  pub mcpu_scaled_ratio: f32,
  /// Middle-tier active residency.
  pub mcpu_active_ratio: f32,
  /// GPU frequency in MHz, averaged over active residency.
  pub gpu_freq_mhz: u32,
  /// GPU active residency weighted by operating frequency relative to its maximum.
  pub gpu_scaled_ratio: f32,
  /// Fraction of the sampling interval spent in active GPU frequency states.
  pub gpu_active_ratio: f32,
  /// CPU package power in Watts, or None when unavailable.
  pub cpu_power: Option<f32>,
  /// GPU power in Watts, or None when unavailable.
  pub gpu_power: Option<f32>,
  /// Apple Neural Engine power in Watts.
  pub ane_power: Option<f32>,
  /// Sum of CPU, GPU, and ANE power in Watts.
  pub all_power: Option<f32>,
  /// System power estimate in Watts, when available.
  pub sys_power: Option<f32>,
  /// DRAM power in Watts.
  pub ram_power: Option<f32>,
  /// GPU SRAM power in Watts.
  pub gpu_ram_power: Option<f32>,
}

// MARK: Helpers

fn is_valid_temp(val: f32) -> bool {
  val > 0.0 && val <= 150.0
}

fn is_valid_fan_rpm(val: f32) -> bool {
  (0.0..=100_000.0).contains(&val)
}

fn fan_rpm_value(val: f32) -> Option<u32> {
  if is_valid_fan_rpm(val) { Some(val.trunc() as u32) } else { None }
}

fn cluster_metrics(cores: &[CpuCoreMetrics], expected: u8) -> FreqMetrics {
  if cores.is_empty() || cores.len() < usize::from(expected) {
    return (0, f32::NAN, f32::NAN);
  }
  let scaled = cores.iter().map(|core| core.scaled_ratio).sum::<f32>() / cores.len() as f32;
  let active = cores.iter().map(|core| core.active_ratio).sum::<f32>() / cores.len() as f32;
  let (sum, count) = cores.iter().filter(|core| core.freq_mhz > 0)
    .fold((0u64, 0u64), |(sum, count), core| (sum + u64::from(core.freq_mhz), count + 1));
  let freq = if scaled.is_finite() { sum.checked_div(count).unwrap_or(0) as u32 } else { 0 };
  (freq, scaled, active)
}

fn aggregate_ioreport_metrics(mut rs: Metrics, soc: &SocInfo) -> Metrics {
  (rs.ecpu_freq_mhz, rs.ecpu_scaled_ratio, rs.ecpu_active_ratio) = cluster_metrics(&rs.ecpu_cores, soc.ecpu_cores);
  (rs.pcpu_freq_mhz, rs.pcpu_scaled_ratio, rs.pcpu_active_ratio) = cluster_metrics(&rs.pcpu_cores, soc.pcpu_cores);
  (rs.mcpu_freq_mhz, rs.mcpu_scaled_ratio, rs.mcpu_active_ratio) = cluster_metrics(&rs.mcpu_cores, soc.mcpu_cores);

  let clusters = [
    (rs.ecpu_cores.len(), soc.ecpu_cores, rs.ecpu_scaled_ratio, rs.ecpu_active_ratio),
    (rs.mcpu_cores.len(), soc.mcpu_cores, rs.mcpu_scaled_ratio, rs.mcpu_active_ratio),
    (rs.pcpu_cores.len(), soc.pcpu_cores, rs.pcpu_scaled_ratio, rs.pcpu_active_ratio),
  ];
  let mut scaled = 0.0;
  let mut active = 0.0;
  let mut total = 0.0;
  for (sampled, expected, scaled_ratio, active_ratio) in clusters {
    let count = sampled.max(usize::from(expected)) as f32;
    if count > 0.0 {
      scaled += scaled_ratio * count;
      active += active_ratio * count;
      total += count;
    }
  }
  rs.cpu_scaled_ratio = scaled / total;
  rs.cpu_active_ratio = active / total;
  rs.all_power = [rs.cpu_power, rs.gpu_power, rs.ane_power].into_iter().sum();
  rs
}

fn smc_numeric_value(data: &[u8], unit: &str) -> Option<f32> {
  match unit {
    "flt " if data.len() == 4 => Some(f32::from_le_bytes(data.try_into().ok()?)),
    "fpe2" if data.len() >= 2 => Some(((data[0] as u16) << 6 | ((data[1] as u16) >> 2)) as f32),
    "ui8 " if !data.is_empty() => Some(data[0] as f32),
    "ui16" if data.len() >= 2 => Some(u16::from_be_bytes(data[0..2].try_into().ok()?) as f32),
    "ui32" if data.len() >= 4 => Some(u32::from_be_bytes(data[0..4].try_into().ok()?) as f32),
    _ => None,
  }
}

fn read_smc_numeric_u32(smc: &mut SMC, key: &str) -> Option<u32> {
  let val = smc.read_val(key).ok()?;
  let val = smc_numeric_value(&val.data, &val.unit)?;
  fan_rpm_value(val)
}

fn calc_freq_from_residencies(items: &[(String, i64)], freqs: &[u32]) -> FreqMetrics {
  // CPU layouts are [IDLE, frequencies...] or [DOWN, IDLE, frequencies...];
  // GPU uses OFF before frequency states.
  let total = items.iter().map(|x| x.1 as f64).sum::<f64>();
  if total <= 0.0 || items.iter().any(|x| x.1 < 0) {
    return (0, f32::NAN, f32::NAN);
  }
  let Some(offset) = items.iter().position(|x| !matches!(x.0.as_str(), "IDLE" | "DOWN" | "OFF"))
  else { return (0, 0.0, 0.0) };
  let usage = items[offset..].iter().map(|x| x.1 as f64).sum::<f64>();
  if usage == 0.0 {
    return (0, 0.0, 0.0);
  }
  if freqs.is_empty() || items.len() - offset < freqs.len()
    || items.iter().skip(offset + freqs.len()).any(|x| x.1 > 0) || freqs.last() == Some(&0) {
    return (0, f32::NAN, (usage / total) as f32);
  }
  let mut avg_freq = 0f64;
  for i in 0..freqs.len() {
    let percent = zero_div(items[i + offset].1 as _, usage);
    avg_freq += percent * freqs[i] as f64;
  }

  let active_ratio = zero_div(usage, total);
  let min_freq = *freqs.first().unwrap() as f64;
  let max_freq = *freqs.last().unwrap() as f64;
  let scaled_ratio = (avg_freq.max(min_freq) * active_ratio) / max_freq;

  (avg_freq as u32, scaled_ratio as f32, active_ratio as f32)
}

fn calc_freq(item: CFDictionaryRef, freqs: &[u32]) -> FreqMetrics {
  calc_freq_from_residencies(&cfio_get_residencies(item), freqs)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CpuCoreKind {
  E,
  M,
  P,
}

fn parse_cpu_core_id(channel: &str, prefix: &str) -> Option<usize> {
  let start = channel.find(prefix)? + prefix.len();
  let digits = channel[start..].chars().take_while(|c| c.is_ascii_digit()).collect::<String>();
  if digits.is_empty() { None } else { digits.parse().ok() }
}

fn parse_die_id(channel: &str) -> usize {
  let Some(rest) = channel.strip_prefix("DIE_") else { return 0 };
  rest.split_once('_').and_then(|(id, _)| id.parse().ok()).unwrap_or(0)
}

fn cpu_core_prefix(channel: &str) -> Option<&'static str> {
  ["PCPU", "ECPU", "MCPU"].into_iter().find(|prefix| channel.contains(prefix))
}

fn cpu_core_sort_key(channel: &str) -> (usize, usize, usize) {
  let die_id = parse_die_id(channel);
  let Some(prefix) = cpu_core_prefix(channel) else { return (die_id, 0, 0) };
  let start = channel.find(prefix).unwrap_or_default() + prefix.len();
  let suffix = &channel[start..];

  if let Some((cluster_id, core_id)) = suffix.split_once("_CPU") {
    let cluster_id = if cluster_id.is_empty() { 0 } else { cluster_id.parse().unwrap_or(0) };
    return (die_id, cluster_id, core_id.parse().unwrap_or(0));
  }

  (die_id, 0, parse_cpu_core_id(channel, prefix).unwrap_or(0))
}

fn parse_cpu_core_channel(channel: &str) -> Option<(CpuCoreKind, CpuCoreKey)> {
  let kind = if channel.contains("PCPU") {
    CpuCoreKind::P
  } else if channel.contains("MCPU") {
    CpuCoreKind::M
  } else if channel.contains("ECPU") {
    CpuCoreKind::E
  } else {
    return None;
  };

  // Ultra channel numbers identify a cluster and a core separately, so the complete channel name
  // is the only collision-free key shared by both regular and Ultra chips.
  Some((kind, channel.to_owned()))
}

fn collect_cpu_core_metrics(metrics: HashMap<CpuCoreKey, FreqMetrics>) -> Vec<CpuCoreMetrics> {
  let mut metrics: Vec<_> = metrics.into_iter().collect();
  metrics.sort_by_key(|(channel, _)| cpu_core_sort_key(channel));
  let mut next_clustered_core_id = HashMap::<usize, usize>::new();

  metrics
    .into_iter()
    .map(|(channel, (freq_mhz, scaled_ratio, active_ratio))| {
      let die_id = parse_die_id(&channel);
      let core_id = if channel.contains("_CPU") {
        let next = next_clustered_core_id.entry(die_id).or_default();
        let core_id = *next;
        *next += 1;
        core_id
      } else {
        cpu_core_prefix(&channel)
          .and_then(|prefix| parse_cpu_core_id(&channel, prefix))
          .unwrap_or_default()
      };

      CpuCoreMetrics { die_id, core_id, freq_mhz, scaled_ratio, active_ratio }
    })
    .collect()
}

fn init_smc() -> WithError<SmcSensors> {
  let mut smc = SMC::new()?;

  let mut cpu_sensors = Vec::new();
  let mut gpu_sensors = Vec::new();
  let mut fan_sensors = Vec::new();

  let names = smc.read_all_keys().unwrap_or(vec![]);
  for name in &names {
    if name.len() == 4 && name.starts_with('F') && name.ends_with("Ac") {
      fan_sensors.push(name.clone());
      continue;
    }
    // Unfortunately, it is not known which keys are responsible for what.
    // Basically in the code that can be found publicly "Tp" is used for CPU and "Tg" for GPU.

    let is_cpu = name.starts_with("Tp") || name.starts_with("Te") || name.starts_with("Ts");
    let is_gpu = name.starts_with("Tg");
    if !is_cpu && !is_gpu {
      continue;
    }

    if smc.read_float_val(name).is_err() {
      continue;
    }

    if is_cpu {
      cpu_sensors.push(name.clone());
    } else if is_gpu {
      gpu_sensors.push(name.clone());
    }
  }

  // Sort first so fan order is stable and any duplicate keys become adjacent for dedup().
  fan_sensors.sort();
  fan_sensors.dedup();

  // println!("{} {}", cpu_sensors.len(), gpu_sensors.len());
  Ok(SmcSensors { smc, cpu_keys: cpu_sensors, gpu_keys: gpu_sensors, fan_keys: fan_sensors })
}

// MARK: Sampler

/// Hardware metrics sampler for Apple Silicon Macs.
///
/// Create one sampler and call [`Sampler::get_metrics`] in a continuous polling
/// loop. Run the sampler in a worker thread when sampling must not block the
/// application thread.
pub struct Sampler {
  soc: SocInfo,
  ior: IOReport,
  hid: Option<IOHIDSensors>,
  smc: Option<SMC>,
  modern_energy: bool,
  smc_cpu_keys: Vec<String>,
  smc_gpu_keys: Vec<String>,
  smc_fan_keys: Vec<String>,
}

impl Sampler {
  /// Initialize hardware metric sources.
  pub fn new() -> WithError<Self> {
    let soc = get_soc_info()?;
    let ior = IOReport::with_filter(Some(ioreport_channels_filter))?;
    let hid = IOHIDSensors::new().ok();
    let (smc, cpu_keys, gpu_keys, fan_keys) = init_smc().ok()
      .map(|s| (Some(s.smc), s.cpu_keys, s.gpu_keys, s.fan_keys)).unwrap_or_default();
    let modern_energy = sysctl_str("kern.osproductversion")
      .and_then(|s| s.split('.').next()?.parse::<u32>().ok()).is_some_and(|major| major >= 27);

    Ok(Sampler {
      soc,
      ior,
      hid,
      smc,
      modern_energy,
      smc_cpu_keys: cpu_keys,
      smc_gpu_keys: gpu_keys,
      smc_fan_keys: fan_keys,
    })
  }

  fn get_temp_smc(&mut self) -> WithError<TempMetrics> {
    let Some(smc) = self.smc.as_mut() else { return Ok(TempMetrics::default()) };
    let mut cpu_metrics = Vec::new();
    for sensor in &self.smc_cpu_keys {
      let Ok(val) = smc.read_float_val(sensor) else { continue };
      if is_valid_temp(val) {
        cpu_metrics.push(val);
      }
    }

    let mut gpu_metrics = Vec::new();
    for sensor in &self.smc_gpu_keys {
      let Ok(val) = smc.read_float_val(sensor) else { continue };
      if is_valid_temp(val) {
        gpu_metrics.push(val);
      }
    }

    let cpu_temp_avg = zero_div(cpu_metrics.iter().sum::<f32>(), cpu_metrics.len() as f32);
    let gpu_temp_avg = zero_div(gpu_metrics.iter().sum::<f32>(), gpu_metrics.len() as f32);

    Ok(TempMetrics { cpu_temp_avg, gpu_temp_avg })
  }

  fn get_temp_hid(&mut self) -> WithError<TempMetrics> {
    let metrics = self.hid.as_ref().map(IOHIDSensors::get_metrics).unwrap_or_default();

    let mut cpu_values = Vec::new();
    let mut gpu_values = Vec::new();

    for (name, value) in &metrics {
      if name.starts_with("pACC MTR Temp Sensor") || name.starts_with("eACC MTR Temp Sensor") {
        // println!("{}: {}", name, value);
        if is_valid_temp(*value) {
          cpu_values.push(*value);
        }
        continue;
      }

      if name.starts_with("GPU MTR Temp Sensor") {
        // println!("{}: {}", name, value);
        if is_valid_temp(*value) {
          gpu_values.push(*value);
        }
        continue;
      }
    }

    let cpu_temp_avg = zero_div(cpu_values.iter().sum(), cpu_values.len() as f32);
    let gpu_temp_avg = zero_div(gpu_values.iter().sum(), gpu_values.len() as f32);

    Ok(TempMetrics { cpu_temp_avg, gpu_temp_avg })
  }

  fn get_temp(&mut self) -> WithError<TempMetrics> {
    let mut temp = self.get_temp_smc()?;
    if !is_valid_temp(temp.cpu_temp_avg) || !is_valid_temp(temp.gpu_temp_avg) {
      let hid = self.get_temp_hid()?;
      if !is_valid_temp(temp.cpu_temp_avg) { temp.cpu_temp_avg = hid.cpu_temp_avg; }
      if !is_valid_temp(temp.gpu_temp_avg) { temp.gpu_temp_avg = hid.gpu_temp_avg; }
    }
    Ok(temp)
  }

  fn get_fans(&mut self) -> Vec<FanMetric> {
    let Some(smc) = self.smc.as_mut() else { return Vec::new() };
    let mut fans = Vec::new();
    for (i, key) in self.smc_fan_keys.iter().enumerate() {
      let Some(rpm) = read_smc_numeric_u32(smc, key) else { continue };
      let name = format!("fan{i}");
      let max_rpm = match key.strip_suffix("Ac") {
        Some(prefix) => {
          read_smc_numeric_u32(smc, &format!("{prefix}Mx")).filter(|rpm| *rpm > 0)
        }
        None => None,
      };
      fans.push(FanMetric { name, rpm, max_rpm });
    }
    fans
  }

  fn get_mem(&mut self) -> WithError<MemMetrics> {
    let (ram_usage, ram_total) = libc_ram()?;
    let (swap_usage, swap_total) = libc_swap()?;
    Ok(MemMetrics { ram_total, ram_usage, swap_total, swap_usage })
  }

  fn get_sys_power(&mut self) -> WithError<f32> {
    self.smc.as_mut().ok_or("SMC unavailable")?.read_float_val("PSTR")
  }

  fn get_ioreport_metrics(
    &self,
    sample: crate::sources::IOReportIterator,
    dt: Duration,
  ) -> WithError<Metrics> {
    let mut ecpu_map: HashMap<CpuCoreKey, FreqMetrics> = HashMap::new();
    let mut pcpu_map: HashMap<CpuCoreKey, FreqMetrics> = HashMap::new();
    let mut mcpu_map: HashMap<CpuCoreKey, FreqMetrics> = HashMap::new();
    let mut rs = Metrics { gpu_active_ratio: f32::NAN, gpu_scaled_ratio: f32::NAN, ..Metrics::default() };
    let mut cpu = PowerSources::default();
    let mut gpu = PowerSources::default();
    let mut ane = PowerSources::default();
    let mut ram = PowerSources::default();

    // Keep this channel handling in sync with ioreport_channels_filter.
    for x in sample {
      if x.group == "CPU Stats" && x.subgroup == CPU_FREQ_CORE_SUBG {
        match parse_cpu_core_channel(&x.channel) {
          Some((CpuCoreKind::P, key)) => {
            let metrics = calc_freq(x.item, &self.soc.pcpu_freqs);
            pcpu_map.insert(key, metrics);
            continue;
          }
          Some((CpuCoreKind::E, key)) => {
            ecpu_map.insert(key, calc_freq(x.item, &self.soc.ecpu_freqs));
            continue;
          }
          Some((CpuCoreKind::M, key)) => {
            if self.soc.mcpu_cores > 0 {
              mcpu_map.insert(key, calc_freq(x.item, &self.soc.mcpu_freqs));
            } else {
              // M5 Pro/Max expose Performance as their lower of two tiers.
              ecpu_map.insert(key, calc_freq(x.item, &self.soc.ecpu_freqs));
            }
            continue;
          }
          None => {}
        }
      }

      if x.group == "GPU Stats" && x.subgroup == GPU_FREQ_DICE_SUBG {
        match x.channel.as_str() {
          "GPUPH" => {
            let (freq, scaled_ratio, active_ratio) = calc_freq(x.item, self.soc.gpu_freqs.get(1..).unwrap_or_default());
            rs.gpu_freq_mhz = freq;
            rs.gpu_scaled_ratio = scaled_ratio;
            rs.gpu_active_ratio = active_ratio;
          }
          _ => {}
        }
      }

      if x.group == "Energy Model" || (x.group == "CLPC" && x.subgroup == "Energy Counters") {
        let value = cfio_watts(x.item, &x.unit, dt).ok();
        let Some(value) = value else { continue };
        let source = match x.channel.as_str() {
          "GPU Energy" => Some(&mut gpu),
          c if c.ends_with("CPU Energy") => Some(&mut cpu),
          c if c.starts_with("ANE") => Some(&mut ane),
          c if c.starts_with("DRAM") => Some(&mut ram),
          c if c.starts_with("GPU SRAM") => {
            add_power(&mut rs.gpu_ram_power, value);
            None
          }
          _ => None,
        };
        if let Some(source) = source {
          let total = if x.group == "CLPC" { &mut source.clpc } else { &mut source.legacy };
          add_power(total, value);
        }
      }
    }
    // On macOS 27 legacy CPU/ANE/DRAM reports can be frozen, zero or catch-up
    // spikes. Without a supported CLPC counter, expose unavailability honestly.
    rs.cpu_power = cpu.reading(!self.modern_energy);
    rs.gpu_power = gpu.reading(true);
    rs.ane_power = ane.reading(!self.modern_energy);
    rs.ram_power = ram.reading(!self.modern_energy);

    rs.ecpu_cores = collect_cpu_core_metrics(ecpu_map);
    rs.mcpu_cores = collect_cpu_core_metrics(mcpu_map);
    rs.pcpu_cores = collect_cpu_core_metrics(pcpu_map);

    Ok(rs)
  }

  /// Collect metrics for the next polling interval.
  ///
  /// Intended to be called continuously in a polling loop. The sampler keeps
  /// an IOReport baseline between calls and derives metrics from the complete
  /// interval between consecutive samples.
  ///
  /// `duration` is the requested polling interval in milliseconds.
  pub fn get_metrics(&mut self, duration: u32) -> WithError<Metrics> {
    // CPU Stats channel naming by chip family (see: https://github.com/vladkens/macmon/issues/47)
    //   M1-M4:  ECPU* = efficiency cores (lower tier)
    //           PCPU* = performance cores (top tier)
    //   M5:     The family has three core tiers, but current chips expose two at a time:
    //             Base:    PCPU* = Super, ECPU* = Efficiency
    //             Pro/Max: PCPU* = Super, MCPU* = Performance
    //           MCPU is a separate middle-tier design, not a renamed ECPU core.
    //   M6:     EACC_ECPU = Efficiency, PACC0_MCPU = Performance,
    //           PACC0_PCPU = Super, all three tiers present simultaneously.
    //   Ultra:  Any-generation Ultra chips prefix channels with "DIE_N_"
    //           and include cluster/core separators (e.g. "DIE_0_PCPU1_CPU0").

    let duration = Duration::from_millis(duration as u64);
    let (sample, elapsed) = self.ior.get_sample_interval(duration)?;
    let mut rs = aggregate_ioreport_metrics(self.get_ioreport_metrics(sample, elapsed)?, &self.soc);

    rs.memory = self.get_mem()?;
    rs.temp = self.get_temp()?;
    rs.fans = self.get_fans();

    rs.sys_power = self.get_sys_power().ok().filter(|value| value.is_finite() && *value > 0.0);

    Ok(rs)
  }

  /// Return static SoC information used by this sampler.
  pub fn get_soc_info(&self) -> &SocInfo {
    &self.soc
  }
}

#[cfg(test)]
mod tests {
  #[test]
  fn power_sources_do_not_double_count_or_invent_zeroes() {
    use super::{PowerSources, add_power};
    assert_eq!(PowerSources::default().reading(true), None);
    assert_eq!(PowerSources { clpc: Some(3.0), legacy: Some(100.0) }.reading(true), Some(3.0));
    assert_eq!(PowerSources { clpc: Some(0.0), legacy: Some(100.0) }.reading(true), Some(0.0));
    assert_eq!(PowerSources { clpc: None, legacy: Some(4.0) }.reading(true), Some(4.0));
    assert_eq!(PowerSources { clpc: None, legacy: Some(0.0) }.reading(false), None);
    let mut total = None;
    for value in [-1.0, f32::NAN, f32::INFINITY] { add_power(&mut total, value); }
    assert_eq!(total, None);
    add_power(&mut total, 0.0);
    assert_eq!(total, Some(0.0));
    add_power(&mut total, 2.5);
    add_power(&mut total, 1.5);
    assert_eq!(total, Some(4.0)); // per-die legacy counters on Ultra
  }

  #[test]
  fn missing_component_does_not_become_a_partial_compute_total() {
    let metrics = super::Metrics { cpu_power: Some(4.0), gpu_power: Some(2.0), ..Default::default() };
    assert_eq!(super::aggregate_ioreport_metrics(metrics, &Default::default()).all_power, None);
  }

  use std::collections::{HashMap, HashSet};

  use crate::sources::SocInfo;

  use super::{
    CpuCoreKind, CpuCoreMetrics, Metrics, aggregate_ioreport_metrics, calc_freq_from_residencies,
    collect_cpu_core_metrics, parse_cpu_core_channel, smc_numeric_value,
  };

  fn core(
    die_id: usize,
    core_id: usize,
    freq_mhz: u32,
    scaled_ratio: f32,
    active_ratio: f32,
  ) -> CpuCoreMetrics {
    CpuCoreMetrics { die_id, core_id, freq_mhz, scaled_ratio, active_ratio }
  }

  fn soc_info(ecpu_cores: u8, pcpu_cores: u8) -> SocInfo {
    SocInfo {
      ecpu_cores,
      pcpu_cores,
      ecpu_freqs: vec![800],
      pcpu_freqs: vec![1800],
      ..Default::default()
    }
  }

  #[test]
  fn counts_all_twelve_m6_cores_in_cpu_load() {
    let soc = SocInfo {
      ecpu_cores: 6, mcpu_cores: 4, pcpu_cores: 2,
      ecpu_freqs: vec![972, 2940], mcpu_freqs: vec![1440, 4788], pcpu_freqs: vec![1440, 4788],
      ..Default::default()
    };
    let metrics = Metrics {
      ecpu_cores: (0..6).map(|i| core(0, i, 972, 0.0, 0.0)).collect(),
      mcpu_cores: (0..4).map(|i| core(0, i, 4788, 1.0, 1.0)).collect(),
      pcpu_cores: (0..2).map(|i| core(0, i, 1440, 0.0, 0.0)).collect(),
      ..Default::default()
    };
    let rs = aggregate_ioreport_metrics(metrics, &soc);
    assert!((rs.cpu_active_ratio - 4.0 / 12.0).abs() < 0.00001);
    assert!((rs.cpu_scaled_ratio - 4.0 / 12.0).abs() < 0.00001);
    assert_eq!((rs.mcpu_freq_mhz, rs.mcpu_active_ratio), (4788, 1.0));
  }

  #[test]
  fn handles_unused_gpu_states_and_unknown_frequency_layouts() {
    let states = vec![("OFF".into(), 50), ("P1".into(), 50), ("P2".into(), 0)];
    assert_eq!(calc_freq_from_residencies(&states, &[502]), (502, 0.5, 0.5));
    assert!(calc_freq_from_residencies(&states, &[]).1.is_nan());
    assert!(calc_freq_from_residencies(&[], &[502]).2.is_nan());
    let missing = vec![("OFF".into(), 50), ("P1".into(), 0), ("P2".into(), 50)];
    let (freq, scaled, active) = calc_freq_from_residencies(&missing, &[502]);
    assert_eq!((freq, active), (0, 0.5));
    assert!(scaled.is_nan());
  }

  #[test]
  fn parse_smc_numeric_values() {
    assert_eq!(smc_numeric_value(&42.5f32.to_le_bytes(), "flt "), Some(42.5));
    assert_eq!(smc_numeric_value(&[0x13, 0x88], "fpe2"), Some(1250.0));
    assert_eq!(smc_numeric_value(&[0x04, 0xd2], "ui16"), Some(1234.0));
    assert_eq!(smc_numeric_value(&[0x00, 0x00, 0x04, 0xd2], "ui32"), Some(1234.0));
  }

  #[test]
  fn aggregates_ioreport_metrics() {
    let rs = aggregate_ioreport_metrics(
      Metrics {
        ecpu_cores: vec![core(0, 0, 2000, 1.0, 1.0)],
        pcpu_cores: vec![core(0, 0, 0, 0.0, 0.0), core(0, 1, 4000, 1.0, 1.0)],
        cpu_power: Some(1.5),
        gpu_power: Some(2.0),
        ane_power: Some(0.5),
        ..Default::default()
      },
      &soc_info(1, 2),
    );

    assert_eq!(rs.ecpu_freq_mhz, 2000);
    assert_eq!(rs.ecpu_scaled_ratio, 1.0);
    assert_eq!(rs.ecpu_active_ratio, 1.0);
    assert_eq!(rs.pcpu_freq_mhz, 4000);
    assert_eq!(rs.pcpu_scaled_ratio, 0.5);
    assert_eq!(rs.pcpu_active_ratio, 0.5);
    assert_eq!(rs.cpu_scaled_ratio, 2.0/3.0);
    assert_eq!(rs.cpu_active_ratio, 2.0/3.0);
    assert_eq!(rs.all_power, Some(4.0));
  }

  #[test]
  fn idle_cluster_has_no_measured_active_frequency() {
    let rs = aggregate_ioreport_metrics(
      Metrics {
        ecpu_cores: vec![core(0, 0, 0, 0.0, 0.0), core(0, 1, 0, 0.0, 0.0)],
        pcpu_cores: vec![core(0, 0, 0, 0.0, 0.0)],
        ..Default::default()
      },
      &soc_info(0, 0),
    );

    assert_eq!(rs.ecpu_freq_mhz, 0);
    assert_eq!(rs.pcpu_freq_mhz, 0);
  }

  #[test]
  fn missing_core_prevents_a_whole_cpu_reading() {
    let rs = aggregate_ioreport_metrics(
      Metrics { ecpu_cores: vec![core(0, 0, 2000, 0.5, 0.5)], ..Default::default() },
      &soc_info(2, 1),
    );
    assert!(rs.cpu_active_ratio.is_nan() && rs.cpu_scaled_ratio.is_nan());
    assert!(rs.ecpu_active_ratio.is_nan() && rs.pcpu_active_ratio.is_nan());
    assert_eq!(rs.ecpu_freq_mhz, 0);
  }

  #[test]
  fn orders_named_core_metrics() {
    let cores = collect_cpu_core_metrics(HashMap::from([
      ("DIE_1_ECPU0".into(), (2000, 0.50, 0.75)),
      ("DIE_0_ECPU1".into(), (1000, 0.25, 0.50)),
    ]));

    assert_eq!((cores[0].die_id, cores[0].core_id), (0, 1));
    assert_eq!((cores[1].die_id, cores[1].core_id), (1, 0));
  }

  #[test]
  fn calculates_frequency_over_the_complete_residency_window() {
    let (frequency, scaled_ratio, active_ratio) = calc_freq_from_residencies(
      &[
        ("DOWN".into(), 0),
        ("IDLE".into(), 500),
        ("1000 MHz".into(), 100),
        ("2000 MHz".into(), 400),
      ],
      &[1000, 2000],
    );

    assert_eq!(frequency, 1800);
    assert!((scaled_ratio - 0.45).abs() < f32::EPSILON);
    assert!((active_ratio - 0.5).abs() < f32::EPSILON);
  }

  #[test]
  fn treats_down_as_dynamic_inactive_residency() {
    let (frequency, scaled_ratio, active_ratio) = calc_freq_from_residencies(
      &[
        ("DOWN".into(), 800),
        ("IDLE".into(), 100),
        ("1000 MHz".into(), 100),
        ("2000 MHz".into(), 0),
      ],
      &[1000, 2000],
    );

    assert_eq!(frequency, 1000);
    assert!((scaled_ratio - 0.05).abs() < f32::EPSILON);
    assert!((active_ratio - 0.1).abs() < f32::EPSILON);
  }

  #[test]
  fn ultra_cpu_channel_matching() {
    // On Ultra chips (M1/M2/M3 Ultra) IOReport CPU Stats channels are prefixed "DIE_N_".
    // The DIE_N prefix must not be mistaken for the core id.
    let cases = [
      ("DIE_0_ECPU0", Some(CpuCoreKind::E)),
      ("DIE_1_ECPU0", Some(CpuCoreKind::E)),
      ("DIE_0_PCPU0", Some(CpuCoreKind::P)),
      ("DIE_1_PCPU0", Some(CpuCoreKind::P)),
      ("ECPU7", Some(CpuCoreKind::E)),
      ("PCPU12", Some(CpuCoreKind::P)),
      ("MCPU3", Some(CpuCoreKind::M)),
      ("EACC_ECPU5", Some(CpuCoreKind::E)),
      ("PACC0_MCPU2", Some(CpuCoreKind::M)),
      ("PACC0_PCPU1", Some(CpuCoreKind::P)),
      ("GPU0", None),
    ];
    for (channel, expected) in cases {
      assert_eq!(
        parse_cpu_core_channel(channel).map(|(kind, _)| kind),
        expected,
        "channel {channel}"
      );
    }
  }

  #[test]
  fn parses_real_m3_ultra_core_channels_without_collisions() {
    let channels = [
      "DIE_0_ECPU_CPU0",
      "DIE_0_ECPU_CPU1",
      "DIE_0_PCPU_CPU0",
      "DIE_0_PCPU_CPU1",
      "DIE_0_PCPU1_CPU0",
      "DIE_0_PCPU1_CPU1",
      "DIE_1_ECPU_CPU0",
      "DIE_1_PCPU_CPU0",
      "DIE_1_PCPU1_CPU0",
    ];
    let parsed = channels.map(parse_cpu_core_channel);

    assert!(parsed.iter().all(Option::is_some));
    let unique = parsed
      .into_iter()
      .flatten()
      .map(|(kind, key)| (kind == CpuCoreKind::P, key))
      .collect::<HashSet<_>>();
    assert_eq!(unique.len(), channels.len());
  }

  #[test]
  fn flattens_real_ultra_cluster_channels_into_core_ids() {
    let cores = collect_cpu_core_metrics(HashMap::from([
      ("DIE_0_PCPU1_CPU0".into(), (2000, 0.50, 0.75)),
      ("DIE_0_PCPU_CPU1".into(), (1100, 0.25, 0.50)),
      ("DIE_0_PCPU_CPU0".into(), (1000, 0.20, 0.40)),
      ("DIE_1_PCPU_CPU0".into(), (1200, 0.30, 0.60)),
    ]));

    assert_eq!(
      cores.iter().map(|core| (core.die_id, core.core_id, core.freq_mhz)).collect::<Vec<_>>(),
      [(0, 0, 1000), (0, 1, 1100), (0, 2, 2000), (1, 0, 1200)]
    );
  }
}
