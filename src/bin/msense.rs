#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("msense работает только на macOS / Apple Silicon.");
}

#[cfg(target_os = "macos")]
mod app {
    use macmon::{Metrics, Sampler, SocInfo};
    use macmon::sources::{IOHIDSensors, SMC};
    use std::env;
    use std::io::{self, Write};

    #[derive(Debug, Clone)]
    struct SensorReading {
        name: String,
        celsius: f32,
    }

    #[derive(Debug, Clone, Default)]
    struct ThermalSnapshot {
        low_cpu: Vec<SensorReading>,
        high_cpu: Vec<SensorReading>,
        other_cpu: Vec<SensorReading>,
        gpu: Vec<SensorReading>,
        source: &'static str,
    }

    impl ThermalSnapshot {
        fn cpu_max(&self) -> Option<f32> {
            self.low_cpu
                .iter()
                .chain(self.high_cpu.iter())
                .chain(self.other_cpu.iter())
                .map(|s| s.celsius)
                .reduce(f32::max)
        }

        fn gpu_max(&self) -> Option<f32> {
            self.gpu.iter().map(|s| s.celsius).reduce(f32::max)
        }
    }

    struct RawTempReader {
        hid: Option<IOHIDSensors>,
        smc: Option<SMC>,
        smc_cpu_keys: Vec<String>,
        smc_gpu_keys: Vec<String>,
    }

    impl RawTempReader {
        fn new() -> Self {
            let hid = IOHIDSensors::new().ok();

            let mut smc = SMC::new().ok();
            let mut smc_cpu_keys = Vec::new();
            let mut smc_gpu_keys = Vec::new();

            if let Some(reader) = smc.as_mut() {
                for name in reader.read_all_keys().unwrap_or_default() {
                    let is_cpu = name.starts_with("Tp") || name.starts_with("Te") || name.starts_with("Ts");
                    let is_gpu = name.starts_with("Tg");
                    if !is_cpu && !is_gpu {
                        continue;
                    }
                    if reader.read_float_val(&name).is_err() {
                        continue;
                    }
                    if is_cpu {
                        smc_cpu_keys.push(name);
                    } else {
                        smc_gpu_keys.push(name);
                    }
                }
                smc_cpu_keys.sort();
                smc_gpu_keys.sort();
            }

            Self { hid, smc, smc_cpu_keys, smc_gpu_keys }
        }

        fn read(&mut self) -> ThermalSnapshot {
            if let Some(hid) = &self.hid {
                let mut out = ThermalSnapshot { source: "IOHID", ..Default::default() };
                for (name, value) in hid.get_metrics() {
                    if !valid_temp(value) {
                        continue;
                    }
                    let reading = SensorReading { name: name.clone(), celsius: value };
                    if name.starts_with("eACC MTR Temp Sensor") {
                        out.low_cpu.push(reading);
                    } else if name.starts_with("pACC MTR Temp Sensor") {
                        out.high_cpu.push(reading);
                    } else if name.starts_with("GPU MTR Temp Sensor") {
                        out.gpu.push(reading);
                    }
                }
                if !out.low_cpu.is_empty() || !out.high_cpu.is_empty() || !out.gpu.is_empty() {
                    sort_sensors(&mut out);
                    return out;
                }
            }

            let mut out = ThermalSnapshot { source: "SMC", ..Default::default() };
            if let Some(smc) = self.smc.as_mut() {
                for key in &self.smc_cpu_keys {
                    let Ok(value) = smc.read_float_val(key) else { continue };
                    if !valid_temp(value) {
                        continue;
                    }
                    let reading = SensorReading { name: key.clone(), celsius: value };
                    if key.starts_with("Te") {
                        out.low_cpu.push(reading);
                    } else if key.starts_with("Tp") {
                        out.high_cpu.push(reading);
                    } else {
                        out.other_cpu.push(reading);
                    }
                }
                for key in &self.smc_gpu_keys {
                    let Ok(value) = smc.read_float_val(key) else { continue };
                    if valid_temp(value) {
                        out.gpu.push(SensorReading { name: key.clone(), celsius: value });
                    }
                }
            }
            sort_sensors(&mut out);
            out
        }
    }

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let mut interval_ms: u32 = 1000;
        let mut once = false;

        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--once" | "-1" => once = true,
                "--interval" | "-i" => {
                    let raw = args.next().ok_or("после --interval нужно число миллисекунд")?;
                    interval_ms = raw.parse::<u32>()?.clamp(200, 10_000);
                }
                "--help" | "-h" => {
                    println!(
                        "msense — монитор Apple Silicon\n\nUSAGE:\n  msense [--interval MS] [--once]\n\nOPTIONS:\n  -i, --interval MS   окно измерения, 200..10000 мс (default: 1000)\n  -1, --once          вывести один снимок и выйти\n  -h, --help          справка"
                    );
                    return Ok(());
                }
                other => return Err(format!("неизвестный аргумент: {other}").into()),
            }
        }

        let soc = SocInfo::new()?;
        let mut sampler = Sampler::new()?;
        let mut raw_temps = RawTempReader::new();

        loop {
            let metrics = sampler.get_metrics(interval_ms)?;
            let temps = raw_temps.read();
            render(&soc, &metrics, &temps, !once)?;
            if once {
                break;
            }
        }

        Ok(())
    }

    fn render(
        soc: &SocInfo,
        m: &Metrics,
        temps: &ThermalSnapshot,
        clear_screen: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if clear_screen {
            print!("\x1b[2J\x1b[H");
        }

        let ram_pct = pct(m.memory.ram_usage, m.memory.ram_total);
        let swap_pct = pct(m.memory.swap_usage, m.memory.swap_total);
        let cpu_max = temps.cpu_max().unwrap_or(m.temp.cpu_temp_avg);
        let gpu_max = temps.gpu_max().unwrap_or(m.temp.gpu_temp_avg);

        println!("MSense  {}  ({})", soc.chip_name, soc.mac_model);
        println!("────────────────────────────────────────────────────────────────────────────────────");
        println!(
            "CPU {:5.1}%   hottest {}   avg {}   power {:6.2} W",
            m.cpu_active_ratio * 100.0,
            temp(cpu_max),
            temp(m.temp.cpu_temp_avg),
            m.cpu_power
        );
        println!(
            "GPU {:5.1}%   {:4} MHz   hottest {}   avg {}   power {:6.2} W   cores {}",
            m.gpu_active_ratio * 100.0,
            m.gpu_freq_mhz,
            temp(gpu_max),
            temp(m.temp.gpu_temp_avg),
            m.gpu_power,
            soc.gpu_cores
        );
        println!(
            "POWER chip(CPU+GPU+ANE) {:6.2} W   system-estimate {}",
            m.all_power,
            if m.sys_power > 0.0 { format!("{:6.2} W", m.sys_power) } else { "n/a".to_string() }
        );
        println!(
            "      CPU {:6.2} W   GPU {:6.2} W   ANE {:6.2} W   DRAM {:6.2} W   GPU-SRAM {:6.2} W",
            m.cpu_power, m.gpu_power, m.ane_power, m.ram_power, m.gpu_ram_power
        );
        println!(
            "RAM {:>6.2} / {:>6.2} GiB ({:5.1}%)   SWAP {:>6.2} / {:>6.2} GiB ({:5.1}%)",
            gib(m.memory.ram_usage),
            gib(m.memory.ram_total),
            ram_pct,
            gib(m.memory.swap_usage),
            gib(m.memory.swap_total),
            swap_pct
        );

        println!("────────────────────────────────────────────────────────────────────────────────────");
        println!(
            "{}-tier avg: {:4} MHz / {:5.1}%    {}-tier avg: {:4} MHz / {:5.1}%",
            soc.ecpu_label,
            m.ecpu_freq_mhz,
            m.ecpu_active_ratio * 100.0,
            soc.pcpu_label,
            m.pcpu_freq_mhz,
            m.pcpu_active_ratio * 100.0,
        );

        println!("────────────────────────────────────────────────────────────────────────────────────");
        println!("CORE    TYPE   FREQ       LOAD(active)   CAPACITY     TEMP SENSOR*");

        for (i, c) in m.ecpu_cores.iter().enumerate() {
            let sensor = temps.low_cpu.get(i).map(|s| s.celsius);
            println!(
                "#{:<2}     {:>2}    {:>4} MHz    {:>6.1}%         {:>6.1}%      {}",
                i + 1,
                soc.ecpu_label,
                c.freq_mhz,
                c.active_ratio * 100.0,
                c.scaled_ratio * 100.0,
                optional_temp(sensor)
            );
        }

        for (i, c) in m.pcpu_cores.iter().enumerate() {
            let sensor = temps.high_cpu.get(i).map(|s| s.celsius);
            println!(
                "#{:<2}     {:>2}    {:>4} MHz    {:>6.1}%         {:>6.1}%      {}",
                i + 1,
                soc.pcpu_label,
                c.freq_mhz,
                c.active_ratio * 100.0,
                c.scaled_ratio * 100.0,
                optional_temp(sensor)
            );
        }

        println!("────────────────────────────────────────────────────────────────────────────────────");
        println!("RAW THERMAL SENSORS ({})", if temps.source.is_empty() { "average fallback" } else { temps.source });
        print_sensor_group(&soc.ecpu_label, &temps.low_cpu);
        print_sensor_group(&soc.pcpu_label, &temps.high_cpu);
        print_sensor_group("CPU", &temps.other_cpu);
        print_sensor_group("GPU", &temps.gpu);

        if temps.low_cpu.is_empty() && temps.high_cpu.is_empty() && temps.other_cpu.is_empty() && temps.gpu.is_empty() {
            println!("  raw sensors unavailable; showing macmon CPU/GPU averages only");
        }

        println!("────────────────────────────────────────────────────────────────────────────────────");
        println!("* TEMP SENSOR is paired by sensor index only; macOS does not guarantee that sensor #N is physical core #N.");
        println!("Temperature colors: <90°C green, 90–99.9°C orange, 100°C+ red. Values are bold.");
        println!("System power is macOS/SMC system estimate; external monitor wall-power is not included reliably.");
        println!("Ctrl-C — выход");

        io::stdout().flush()?;
        Ok(())
    }

    fn print_sensor_group(label: &str, sensors: &[SensorReading]) {
        for (i, s) in sensors.iter().enumerate() {
            println!("  {:>3} sensor #{:<2}  {}   {}", label, i + 1, temp(s.celsius), s.name);
        }
    }

    fn temp(value: f32) -> String {
        const RESET: &str = "\x1b[0m";
        const GREEN_BOLD: &str = "\x1b[1;32m";
        const ORANGE_BOLD: &str = "\x1b[1;38;5;208m";
        const RED_BOLD: &str = "\x1b[1;31m";

        let style = if value >= 100.0 {
            RED_BOLD
        } else if value >= 90.0 {
            ORANGE_BOLD
        } else {
            GREEN_BOLD
        };
        format!("{style}{value:5.1}°C{RESET}")
    }

    fn optional_temp(value: Option<f32>) -> String {
        value.map(temp).unwrap_or_else(|| "  n/a  ".to_string())
    }

    fn valid_temp(value: f32) -> bool {
        value > 0.0 && value <= 150.0
    }

    fn sort_sensors(t: &mut ThermalSnapshot) {
        t.low_cpu.sort_by(|a, b| a.name.cmp(&b.name));
        t.high_cpu.sort_by(|a, b| a.name.cmp(&b.name));
        t.other_cpu.sort_by(|a, b| a.name.cmp(&b.name));
        t.gpu.sort_by(|a, b| a.name.cmp(&b.name));
    }

    fn gib(bytes: u64) -> f64 {
        bytes as f64 / 1024.0 / 1024.0 / 1024.0
    }

    fn pct(used: u64, total: u64) -> f64 {
        if total == 0 { 0.0 } else { used as f64 * 100.0 / total as f64 }
    }
}

#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    app::run()
}
