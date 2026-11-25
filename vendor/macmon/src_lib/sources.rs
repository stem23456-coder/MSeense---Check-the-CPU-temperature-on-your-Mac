//! Low-level Apple Silicon metric sources.
//!
//! This module exposes the SMC, IOReport, IOHID, CoreFoundation, and system
//! profiler helpers used by [`crate::Sampler`]. It is useful for advanced
//! integrations, debugging new hardware, or experimenting with additional Apple
//! Silicon counters before they are added to the high-level metrics API.
//!
//! Prefer [`crate::Sampler`] for regular use. The APIs in this module are lower
//! level and may change between releases as macOS, hardware generations, and
//! metric keys change.

#![allow(non_upper_case_globals)]
//! Low-level macOS metric sources.

#![allow(dead_code)]

use std::{
  collections::HashMap,
  ffi::CString,
  marker::{PhantomData, PhantomPinned},
  mem::{MaybeUninit, size_of},
  os::raw::c_void,
  ptr::{null, null_mut},
  sync::OnceLock,
  time::Duration,
};

use core_foundation::{
  array::{
    CFArray, CFArrayAppendValue, CFArrayCreateMutable, CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef,
    CFMutableArrayRef, kCFTypeArrayCallBacks,
  },
  base::{CFAllocatorRef, CFRange, CFRelease, CFType, CFTypeRef, TCFType, kCFAllocatorDefault, kCFAllocatorNull},
  data::{CFDataGetBytes, CFDataGetLength, CFDataRef},
  dictionary::{
    CFDictionary, CFMutableDictionary, CFDictionaryCreateMutableCopy, CFDictionaryGetCount,
    CFDictionaryGetKeysAndValues, CFDictionaryGetValue, CFDictionaryRef, CFDictionarySetValue,
    CFMutableDictionaryRef,
  },
  number::{
    CFNumber, CFNumberCreate, CFNumberGetValue, CFNumberRef, kCFNumberSInt32Type, kCFNumberSInt64Type,
  },
  string::{CFString, CFStringCreateWithBytesNoCopy, CFStringRef, kCFStringEncodingUTF8},
};
use serde::Serialize;

/// Error type used by low-level source helpers.
pub type WithError<T> = Result<T, Box<dyn std::error::Error>>;
/// Raw CoreFoundation/IOKit pointer used by FFI bindings.
pub type CVoidRef = *const std::ffi::c_void;

static SOC_INFO_CACHE: OnceLock<SocInfo> = OnceLock::new();

// MARK: CFUtils

/// Create a CoreFoundation number object from an `i32`.
pub fn cfnum(val: i32) -> CFNumberRef {
  unsafe { CFNumberCreate(kCFAllocatorDefault, kCFNumberSInt32Type, &val as *const i32 as _) }
}

/// Create a CoreFoundation string object from a Rust string.
pub fn cfstr(val: &str) -> CFStringRef {
  // this creates broken objects if string len > 9
  // CFString::from_static_string(val).as_concrete_TypeRef()
  // CFString::new(val).as_concrete_TypeRef()

  unsafe {
    CFStringCreateWithBytesNoCopy(
      kCFAllocatorDefault,
      val.as_ptr(),
      val.len() as isize,
      kCFStringEncodingUTF8,
      0,
      kCFAllocatorNull,
    )
  }
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
/// Convert a CoreFoundation string reference into a Rust `String`.
pub fn from_cfstr(val: CFStringRef) -> String {
  if val.is_null() { return String::new(); }
  // CFString handles arbitrarily long and non-ASCII sensor names.
  unsafe { CFString::wrap_under_get_rule(val) }.to_string()
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
/// Return all keys from a CoreFoundation dictionary as Rust strings.
pub fn cfdict_keys(dict: CFDictionaryRef) -> Vec<String> {
  unsafe {
    let count = CFDictionaryGetCount(dict) as usize;
    let mut keys: Vec<CFStringRef> = Vec::with_capacity(count);
    let mut vals: Vec<CFTypeRef> = Vec::with_capacity(count);
    CFDictionaryGetKeysAndValues(dict, keys.as_mut_ptr() as _, vals.as_mut_ptr());
    keys.set_len(count);
    vals.set_len(count);

    keys.iter().map(|k| from_cfstr(*k as _)).collect()
  }
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
/// Look up a value in a CoreFoundation dictionary by string key.
pub fn cfdict_get_val(dict: CFDictionaryRef, key: &str) -> Option<CFTypeRef> {
  unsafe {
    let key = cfstr(key);
    let val = CFDictionaryGetValue(dict, key as _);
    CFRelease(key as _);

    match val {
      _ if val.is_null() => None,
      _ => Some(val),
    }
  }
}

// MARK: IOReport Bindings

#[link(name = "IOKit", kind = "framework")]
#[rustfmt::skip]
unsafe extern "C" {
  fn IOServiceMatching(name: *const i8) -> CFMutableDictionaryRef;
  fn IOServiceGetMatchingServices(mainPort: u32, matching: CFDictionaryRef, existing: *mut u32) -> i32;
  fn IOIteratorNext(iterator: u32) -> u32;
  fn IORegistryEntryGetRegistryEntryID(entry: u32, id: *mut u64) -> i32;
  fn IORegistryEntryGetName(entry: u32, name: *mut i8) -> i32;
  fn IORegistryEntryCreateCFProperties(entry: u32, properties: *mut CFMutableDictionaryRef, allocator: CFAllocatorRef, options: u32) -> i32;
  fn IOObjectRelease(obj: u32) -> u32;
}

#[repr(C)]
struct IOReportSubscription {
  _data: [u8; 0],
  _phantom: PhantomData<(*mut u8, PhantomPinned)>,
}

type IOReportSubscriptionRef = *const IOReportSubscription;
type ChannelFilter = fn(&str, &str, &str, &str) -> bool;
type ChannelFilterRef<'a> = &'a dyn Fn(&str, &str, &str, &str) -> bool;

#[link(name = "IOReport", kind = "dylib")]
#[rustfmt::skip]
unsafe extern "C" {
  fn IOReportCopyAllChannels(a: u64, b: u64) -> CFDictionaryRef;
  fn IOReportCreateSubscription(a: CVoidRef, b: CFMutableDictionaryRef, c: *mut CFMutableDictionaryRef, d: u64, b: CFTypeRef) -> IOReportSubscriptionRef;
  fn IOReportCreateSamples(a: IOReportSubscriptionRef, b: CFMutableDictionaryRef, c: CFTypeRef) -> CFDictionaryRef;
  fn IOReportCreateSamplesDelta(a: CFDictionaryRef, b: CFDictionaryRef, c: CFTypeRef) -> CFDictionaryRef;
  fn IOReportChannelGetDriverID(a: CFDictionaryRef) -> u64;
  fn IOReportChannelGetChannelID(a: CFDictionaryRef) -> u64;
  fn IOReportChannelGetGroup(a: CFDictionaryRef) -> CFStringRef;
  fn IOReportChannelGetSubGroup(a: CFDictionaryRef) -> CFStringRef;
  fn IOReportChannelGetChannelName(a: CFDictionaryRef) -> CFStringRef;
  fn IOReportSimpleGetIntegerValue(a: CFDictionaryRef, b: i32) -> i64;
  fn IOReportChannelGetUnitLabel(a: CFDictionaryRef) -> CFStringRef;
  fn IOReportStateGetCount(a: CFDictionaryRef) -> i32;
  fn IOReportStateGetNameForIndex(a: CFDictionaryRef, b: i32) -> CFStringRef;
  fn IOReportStateGetResidency(a: CFDictionaryRef, b: i32) -> i64;
}

// MARK: IOReport helpers

fn cfio_get_group(item: CFDictionaryRef) -> String {
  match unsafe { IOReportChannelGetGroup(item) } {
    x if x.is_null() => String::new(),
    x => from_cfstr(x),
  }
}

fn cfio_get_subgroup(item: CFDictionaryRef) -> String {
  match unsafe { IOReportChannelGetSubGroup(item) } {
    x if x.is_null() => String::new(),
    x => from_cfstr(x),
  }
}

fn cfio_get_channel(item: CFDictionaryRef) -> String {
  match unsafe { IOReportChannelGetChannelName(item) } {
    x if x.is_null() => String::new(),
    x => from_cfstr(x),
  }
}

fn cfio_channel_matches(items: &[(&str, Option<&str>)], group: &str, subgroup: &str) -> bool {
  items.is_empty()
    || items.iter().any(|(item_group, item_subgroup)| {
      *item_group == group && item_subgroup.is_none_or(|value| value == subgroup)
    })
}

/// Copy all CoreFoundation properties from an IORegistry entry.
pub fn cfio_get_props(entry: IOService, name: String) -> WithError<CFDictionaryRef> {
  unsafe {
    let mut props: MaybeUninit<CFMutableDictionaryRef> = MaybeUninit::uninit();
    if IORegistryEntryCreateCFProperties(entry.raw(), props.as_mut_ptr(), kCFAllocatorDefault, 0) != 0 {
      return Err(format!("Failed to get properties for {}", name).into());
    }

    Ok(props.assume_init())
  }
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
/// Read IOReport state residency counters from a channel item.
pub fn cfio_get_residencies(item: CFDictionaryRef) -> Vec<(String, i64)> {
  let count = unsafe { IOReportStateGetCount(item) };
  let mut res = vec![];

  for i in 0..count {
    let name = unsafe { IOReportStateGetNameForIndex(item, i) };
    let val = unsafe { IOReportStateGetResidency(item, i) };
    let name = match name {
      x if x.is_null() => format!("S{i}"),
      x => from_cfstr(x),
    };
    res.push((name, val));
  }

  res
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
/// Convert an IOReport energy counter into Watts for a sampling duration.
pub fn cfio_watts(item: CFDictionaryRef, unit: &str, duration: Duration) -> WithError<f32> {
  let val = unsafe { IOReportSimpleGetIntegerValue(item, 0) } as f64;
  watts_from_energy(val, unit, duration)
}

fn watts_from_energy(val: f64, unit: &str, duration: Duration) -> WithError<f32> {
  let val = val / duration.as_secs_f64();
  match unit {
    "mJ" => Ok((val / 1e3) as f32),
    "uJ" => Ok((val / 1e6) as f32),
    "nJ" => Ok((val / 1e9) as f32),
    _ => Err(format!("Invalid energy unit: {}", unit).into()),
  }
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
/// Read the integer value from an IOReport channel item.
pub fn cfio_integer_value(item: CFDictionaryRef) -> i64 {
  unsafe { IOReportSimpleGetIntegerValue(item, 0) }
}

// MARK: IOServiceIterator

/// Owned reference returned by IOIteratorNext.
pub struct IOService(u32);

impl IOService {
  fn raw(&self) -> u32 { self.0 }
}

impl Drop for IOService {
  fn drop(&mut self) {
    unsafe { IOObjectRelease(self.0); }
  }
}

/// Iterator over IORegistry services matching a service name.
pub struct IOServiceIterator {
  existing: u32,
}

impl IOServiceIterator {
  /// Create an iterator for services such as `AppleSMC` or `AppleARMIODevice`.
  pub fn new(service_name: &str) -> WithError<Self> {
    let service_name = std::ffi::CString::new(service_name).unwrap();
    let existing = unsafe {
      let service = IOServiceMatching(service_name.as_ptr() as _);
      let mut existing = 0;
      if IOServiceGetMatchingServices(0, service, &mut existing) != 0 {
        return Err(format!("{} not found", service_name.to_string_lossy()).into());
      }
      existing
    };

    Ok(Self { existing })
  }
}

impl Drop for IOServiceIterator {
  fn drop(&mut self) {
    unsafe {
      IOObjectRelease(self.existing);
    }
  }
}

impl Iterator for IOServiceIterator {
  type Item = (IOService, String);

  fn next(&mut self) -> Option<Self::Item> {
    let next = unsafe { IOIteratorNext(self.existing) };
    if next == 0 {
      return None;
    }

    let entry = IOService(next);
    let mut name = [0; 128]; // 128 defined in apple docs
    if unsafe { IORegistryEntryGetName(next, name.as_mut_ptr()) } != 0 {
      return None;
    }

    let name = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) };
    let name = name.to_string_lossy().to_string();
    Some((entry, name))
  }
}

// MARK: IOReportIterator

/// Iterator over IOReport channel samples.
pub type IOReportIterator = std::vec::IntoIter<IOReportIteratorItem>;

#[derive(Debug)]
/// One IOReport channel item returned by [`IOReportIterator`].
pub struct IOReportIteratorItem {
  /// IOReport group name.
  pub group: String,
  /// IOReport subgroup name.
  pub subgroup: String,
  /// IOReport channel name.
  pub channel: String,
  /// IOReport unit label.
  pub unit: String,
  /// Channel dictionary, valid while this item is alive.
  pub item: CFDictionaryRef,
  _owner: CFDictionary,
}

// Only called with Create-rule IOReport results.
unsafe fn take_sample(raw: CFDictionaryRef) -> WithError<CFDictionary> {
  if raw.is_null() { return Err("IOReport returned no sample".into()); }
  Ok(unsafe { CFDictionary::wrap_under_create_rule(raw) })
}

fn channel_array(sample: CFDictionaryRef) -> WithError<CFArray> {
  let raw = cfdict_get_val(sample, "IOReportChannels")
    .ok_or("IOReport sample has no channels array")?;
  unsafe { CFType::wrap_under_get_rule(raw) }.downcast_into::<CFArray>()
    .ok_or_else(|| "IOReport channels is not an array".into())
}

fn parse_sample(sample: CFDictionary) -> WithError<IOReportIterator> {
  let channels = channel_array(sample.as_concrete_TypeRef())?;
  let mut result = Vec::with_capacity(channels.len() as usize);
  // The sample can omit or reorder channels relative to the subscription.
  for raw in channels.iter() {
    let owner = unsafe { CFType::wrap_under_get_rule(*raw) }.downcast_into::<CFDictionary>()
      .ok_or("IOReport channel is not a dictionary")?;
    let item = owner.as_concrete_TypeRef();
    result.push(IOReportIteratorItem {
      group: cfio_get_group(item),
      subgroup: cfio_get_subgroup(item),
      channel: cfio_get_channel(item),
      unit: from_cfstr(unsafe { IOReportChannelGetUnitLabel(item) }).trim().to_string(),
      item,
      _owner: owner,
    });
  }
  Ok(result.into_iter())
}

// MARK: RAM

// HOST_VM_INFO64 includes speculative pages in free_count. File-backed
// pages already include them, so subtract only the remaining free pages.
fn used_ram_bytes(total: u64, free: u32, speculative: u32, external: u32, page_size: u64) -> u64 {
  let available_pages = u64::from(free.saturating_sub(speculative)) + u64::from(external);
  total.saturating_sub(available_pages.saturating_mul(page_size))
}

/// Read used and total RAM in bytes via libc/mach APIs.
pub fn libc_ram() -> WithError<(u64, u64)> {
  let mut total = 0u64;
  let usage;

  unsafe {
    let mut name = [libc::CTL_HW, libc::HW_MEMSIZE];
    let mut size = std::mem::size_of::<u64>();
    let ret_code = libc::sysctl(
      name.as_mut_ptr(),
      name.len() as _,
      &mut total as *mut _ as *mut _,
      &mut size,
      std::ptr::null_mut(),
      0,
    );

    if ret_code != 0 {
      return Err("Failed to get total memory".into());
    }
  }

  unsafe {
    let mut count: u32 = libc::HOST_VM_INFO64_COUNT as _;
    let mut stats = std::mem::zeroed::<libc::vm_statistics64>();

    // todo: https://github.com/JohnTitor/mach2/issues/34
    #[allow(deprecated)]
    let host = libc::mach_host_self();
    let ret_code = libc::host_statistics64(
      host,
      libc::HOST_VM_INFO64,
      &mut stats as *mut _ as *mut _,
      &mut count,
    );

    mach_port_deallocate(mach_task_self(), host);

    if ret_code != 0 {
      return Err("Failed to get memory stats".into());
    }

    let page_size = libc::sysconf(libc::_SC_PAGESIZE);
    if page_size <= 0 {
      return Err("Failed to get VM page size".into());
    }
    usage = used_ram_bytes(total, stats.free_count, stats.speculative_count, stats.external_page_count, page_size as u64);
  }

  Ok((usage, total))
}

/// Read used and total swap in bytes via libc sysctl.
pub fn libc_swap() -> WithError<(u64, u64)> {
  let (mut usage, mut total) = (0u64, 0u64);

  unsafe {
    let mut name = [libc::CTL_VM, libc::VM_SWAPUSAGE];
    let mut size = std::mem::size_of::<libc::xsw_usage>();
    let mut xsw: libc::xsw_usage = std::mem::zeroed::<libc::xsw_usage>();

    let ret_code = libc::sysctl(
      name.as_mut_ptr(),
      name.len() as _,
      &mut xsw as *mut _ as *mut _,
      &mut size,
      std::ptr::null_mut(),
      0,
    );

    if ret_code != 0 {
      return Err("Failed to get swap usage".into());
    }

    usage = xsw.xsu_used;
    total = xsw.xsu_total;
  }

  Ok((usage, total))
}

// MARK: SockInfo

/// Static Apple Silicon system information.
///
/// This describes the current machine and the frequency tables used to interpret
/// CPU/GPU residency counters.
#[derive(Debug, Default, Clone, Serialize)]
pub struct SocInfo {
  /// Apple hardware model identifier, such as `Mac14,7`.
  pub mac_model: String,
  /// Human-readable chip name.
  pub chip_name: String,
  /// Installed unified memory size in GiB.
  pub memory_gb: u16,
  /// Number of efficiency-tier CPU cores.
  pub ecpu_cores: u8,
  /// Number of performance-tier CPU cores.
  pub pcpu_cores: u8,
  /// Middle-tier Performance cores on three-tier chips (M6).
  pub mcpu_cores: u8,
  /// UI label for the lower CPU tier, for example `E` on M1-M4 or `P` on M5+.
  pub ecpu_label: String,
  /// UI label for the higher CPU tier, for example `P` on M1-M4 or `S` on M5+.
  pub pcpu_label: String,
  /// Supported lower-tier CPU frequencies in MHz.
  pub ecpu_freqs: Vec<u32>,
  /// Supported higher-tier CPU frequencies in MHz.
  pub pcpu_freqs: Vec<u32>,
  /// Frequencies of the middle tier in MHz, when present.
  pub mcpu_freqs: Vec<u32>,
  /// Number of GPU cores.
  pub gpu_cores: u8,
  /// Supported GPU frequencies in MHz.
  pub gpu_freqs: Vec<u32>,
}

impl SocInfo {
  /// Load static SoC information for the current machine.
  pub fn new() -> WithError<Self> {
    // Keep this constructor for external library users; internal call sites use get_soc_info().
    get_soc_info()
  }
}

/// Parse dynamic voltage and frequency scaling data from an IORegistry dictionary.
pub fn get_dvfs_mhz(dict: CFDictionaryRef, key: &str) -> Option<(Vec<u32>, Vec<u32>)> {
  unsafe {
    let obj = cfdict_get_val(dict, key)? as CFDataRef;
    let obj_len = CFDataGetLength(obj);
    let mut obj_val = vec![0u8; obj_len as usize];
    CFDataGetBytes(obj, CFRange::init(0, obj_len), obj_val.as_mut_ptr());

    // obj_val is pairs of (freq, voltage) 4 bytes each
    let items_count = (obj_len / 8) as usize;
    let [mut freqs, mut volts] = [vec![0u32; items_count], vec![0u32; items_count]];
    for (i, x) in obj_val.chunks_exact(8).enumerate() {
      volts[i] = u32::from_le_bytes([x[4], x[5], x[6], x[7]]);
      freqs[i] = u32::from_le_bytes([x[0], x[1], x[2], x[3]]);
    }

    Some((volts, freqs))
  }
}

// Parse acc-clusters bytes into (ecpu_key, pcpu_key) voltage-states key names.
// Each 8-byte entry: byte 0 = voltage-states index, byte 1 = cluster type
// (0 = efficiency/lowest tier, higher = higher perf tier).
// Picks highest type as pcpu, second-highest as ecpu — handles M5 Max where
// type 0 (E-core cluster) is absent and the two active tiers are 1 and 2.
fn parse_acc_clusters(data: &[u8]) -> Option<(String, String)> {
  let mut clusters: Vec<(u8, String)> = Vec::new();
  for chunk in data.chunks_exact(8) {
    clusters.push((chunk[1], format!("voltage-states{}-sram", chunk[0])));
  }
  clusters.sort_by_key(|c| c.0);
  if clusters.len() < 2 {
    return None;
  }
  let ecpu_key = clusters[clusters.len() - 2].1.clone();
  let pcpu_key = clusters.last()?.1.clone();
  Some((ecpu_key, pcpu_key))
}

// Read acc-clusters from pmgr dict and parse into (ecpu_key, pcpu_key).
fn parse_acc_clusters_from(dict: CFDictionaryRef) -> Option<(String, String)> {
  let obj = cfdict_get_val(dict, "acc-clusters")? as CFDataRef;

  let len = unsafe { CFDataGetLength(obj) } as usize;
  if len < 8 {
    return None;
  }

  let mut data = vec![0u8; len];
  unsafe { CFDataGetBytes(obj, CFRange::init(0, len as _), data.as_mut_ptr()) };

  parse_acc_clusters(&data)
}

fn to_mhz(vals: Vec<u32>, scale: u32) -> Vec<u32> {
  vals.iter().map(|x| *x / scale).collect()
}

// M1–M3 and A-series chips store frequencies in Hz; M4+ store in kHz.
fn cpu_freq_scale(chip_name: &str) -> u32 {
  let hz_freqs = chip_name.contains("M1")
    || chip_name.contains("M2")
    || chip_name.contains("M3")
    || chip_name.contains("A1"); // A14–A18 and future A1x chips
  if hz_freqs { 1_000_000 } else { 1_000 }
}

// Try known voltage-states key (M1-M4) first, fall back to acc-clusters discovery (M5+).
fn cpu_freqs(item: CFDictionaryRef, key: &str, is_ecpu: bool, scale: u32) -> Option<Vec<u32>> {
  if let Some((_, freqs)) = get_dvfs_mhz(item, key) {
    return Some(to_mhz(freqs, scale));
  }
  let (ecpu_key, pcpu_key) = parse_acc_clusters_from(item)?;
  let key = if is_ecpu { ecpu_key } else { pcpu_key };
  let (_, freqs) = get_dvfs_mhz(item, &key)?;
  Some(to_mhz(freqs, scale))
}

// Parse "proc T:P:E" (macOS 15) or "proc T:P_or_S:E:M" (macOS 26+) into (ecpu, pcpu, has_mcpu).
// macOS 26 always uses 4 fields; M5+ has M>0 (ecpu=M, pcpu=S), M1-M4 has M=0 (ecpu=E, pcpu=P).
fn parse_cpu_cores(s: &str) -> (u64, u64, bool) {
  let procs = s.strip_prefix("proc ").unwrap_or("");
  let parts: Vec<u64> = procs.split(':').map(|x| x.parse().unwrap_or(0)).collect();

  match parts.len() {
    4 => {
      let (e, m) = (parts[2], parts[3]);
      if m > 0 { (m, parts[1], true) } else { (e, parts[1], false) }
    }
    3 => (parts[2], parts[1], false), // macOS 15: "proc total:P:E"
    _ => (0, 0, false),
  }
}

// Static hardware fields obtainable either via `system_profiler` or directly via
// sysctl/IOKit. Kept separate from SocInfo so the two sourcing strategies below
// don't need to touch the CPU/GPU frequency tables, which are always read via IOKit.
#[derive(Debug)]
pub(crate) struct HwInfo {
  pub(crate) chip_name: String,
  pub(crate) mac_model: String,
  pub(crate) memory_gb: u16,
  pub(crate) ecpu_cores: u8,
  pub(crate) pcpu_cores: u8,
  pub(crate) mcpu_cores: u8,
  pub(crate) ecpu_label: String,
  pub(crate) pcpu_label: String,
  pub(crate) gpu_cores: u8,
}

/// Read a string sysctl value by name.
pub(crate) fn sysctl_str(name: &str) -> Option<String> {
  let cname = CString::new(name).ok()?;
  unsafe {
    let mut size: usize = 0;
    let ret = libc::sysctlbyname(cname.as_ptr(), null_mut(), &mut size, null_mut(), 0);
    if ret != 0 || size == 0 {
      return None;
    }

    let mut buf = vec![0u8; size];
    let ret =
      libc::sysctlbyname(cname.as_ptr(), buf.as_mut_ptr() as *mut c_void, &mut size, null_mut(), 0);
    if ret != 0 {
      return None;
    }

    buf.truncate(size.saturating_sub(1)); // drop trailing NUL
    String::from_utf8(buf).ok()
  }
}

fn sysctl_buf<const N: usize>(name: &str) -> Option<[u8; N]> {
  let cname = CString::new(name).ok()?;
  let mut buf = [0; N];
  let mut size = N;
  let ret = unsafe {
    libc::sysctlbyname(cname.as_ptr(), buf.as_mut_ptr().cast(), &mut size, null_mut(), 0)
  };
  (ret == 0).then_some(buf)
}

fn sysctl_u32(name: &str) -> Option<u32> {
  sysctl_buf(name).map(u32::from_ne_bytes)
}

fn sysctl_u64(name: &str) -> Option<u64> {
  sysctl_buf(name).map(u64::from_ne_bytes)
}

/// Read an integer CFNumber property from an IORegistry properties dictionary.
fn cfnum_get_i64(dict: CFDictionaryRef, key: &str) -> Option<i64> {
  let obj = cfdict_get_val(dict, key)? as CFNumberRef;
  let mut val: i64 = 0;
  let ok = unsafe { CFNumberGetValue(obj, kCFNumberSInt64Type, &mut val as *mut _ as *mut c_void) };
  ok.then_some(val)
}

// perflevels are highest first. Read their names: base M5 is E/S, M5 Pro/Max
// is P/S, and M6 exposes all three tiers as Super / Performance / Efficiency.
fn topology_from_perflevels(
  cores: &[u32], names: &[Option<String>], chip_name: &str,
) -> Option<(u8, u8, u8, &'static str, &'static str)> {
  let (hi, mid, lo) = match *cores {
    [hi, lo] => (hi, 0, lo),
    [hi, mid, lo] => (hi, mid, lo),
    _ => return None,
  };
  if hi == 0 || lo == 0 || [hi, mid, lo].iter().any(|n| *n > u8::MAX as u32) {
    return None;
  }
  let is_legacy = ["M1", "M2", "M3", "M4", "A1"].iter().any(|x| chip_name.contains(x));
  let labels = if is_legacy { ("E", "P") } else if mid > 0 { ("E", "S") } else { ("P", "S") };
  let label = |name: Option<&Option<String>>| match name?.as_deref()? {
    "Efficiency" => Some("E"), "Performance" => Some("P"), "Super" => Some("S"), _ => None,
  };
  Some((lo as u8, mid as u8, hi as u8,
    label(names.last()).unwrap_or(labels.0), label(names.first()).unwrap_or(labels.1)))
}

fn cpu_tier_counts(chip_name: &str) -> Option<(u8, u8, u8, &'static str, &'static str)> {
  let count = sysctl_u32("hw.nperflevels")?;
  let cores = (0..count).map(|i| sysctl_u32(&format!("hw.perflevel{i}.physicalcpu")))
    .collect::<Option<Vec<_>>>()?;
  let names = (0..count).map(|i| sysctl_str(&format!("hw.perflevel{i}.name"))).collect::<Vec<_>>();
  topology_from_perflevels(&cores, &names, chip_name)
}

/// Read hardware descriptor fields via sysctl and IORegistry only (no subprocess).
pub(crate) fn hw_native() -> WithError<HwInfo> {
  let chip_name = sysctl_str("machdep.cpu.brand_string").ok_or("Failed to read chip name")?;
  let mac_model = sysctl_str("hw.model").ok_or("Failed to read mac model")?;
  let memory_gb =
    sysctl_u64("hw.memsize").ok_or("Failed to read memory size")? / (1024 * 1024 * 1024);
  let (ecpu_cores, mcpu_cores, pcpu_cores, ecpu_label, pcpu_label) =
    cpu_tier_counts(&chip_name).ok_or("Failed to read CPU core topology")?;

  let mut gpu_cores = 0u8;
  for (entry, name) in IOServiceIterator::new("AGXAccelerator")? {
    if let Ok(item) = cfio_get_props(entry, name) {
      if let Some(cores) = cfnum_get_i64(item, "gpu-core-count") {
        gpu_cores = cores as u8;
      }
      unsafe { CFRelease(item as _) }
    }
  }

  Ok(HwInfo {
    chip_name,
    mac_model,
    memory_gb: memory_gb as u16,
    ecpu_cores,
    mcpu_cores,
    pcpu_cores,
    ecpu_label: ecpu_label.into(),
    pcpu_label: pcpu_label.into(),
    gpu_cores,
  })
}

/// Read hardware descriptor fields via `system_profiler` (slower, ~250-300ms subprocess
/// spawn, but battle-tested against real M1-M5 hardware bug reports).
pub(crate) fn hw_from_profiler() -> WithError<HwInfo> {
  let out = std::process::Command::new("system_profiler")
    .args(["SPHardwareDataType", "SPDisplaysDataType", "-json"])
    .output()?;
  let out = std::str::from_utf8(&out.stdout)?;
  let out = serde_json::from_str::<serde_json::Value>(out)?;

  // SPHardwareDataType.0.chip_type
  let chip_name = out["SPHardwareDataType"][0]["chip_type"].as_str();
  let chip_name = chip_name.unwrap_or("Unknown chip").to_string();

  // SPHardwareDataType.0.machine_model
  let mac_model = out["SPHardwareDataType"][0]["machine_model"].as_str();
  let mac_model = mac_model.unwrap_or("Unknown model").to_string();

  // SPHardwareDataType.0.physical_memory -> "x GB"
  let mem_gb = out["SPHardwareDataType"][0]["physical_memory"].as_str();
  let mem_gb = mem_gb.and_then(|x| x.strip_suffix(" GB")).and_then(|x| x.parse::<u64>().ok());
  let mem_gb = mem_gb.unwrap_or(0);

  // SPHardwareDataType.0.number_processors -> "proc x:y:z" or "proc x:y:z:w"
  let number_processors = out["SPHardwareDataType"][0]["number_processors"].as_str().unwrap_or("");
  let os_major = sysctl_str("kern.osproductversion")
    .and_then(|s| s.split('.').next()?.parse::<u32>().ok()).unwrap_or(0);
  let (ecpu_cores, mcpu_cores, pcpu_cores, ecpu_label, pcpu_label) =
    profiler_topology(number_processors, os_major);

  // SPDisplaysDataType.0.sppci_cores
  let gpu_cores = out["SPDisplaysDataType"][0]["sppci_cores"].as_str();
  let gpu_cores = gpu_cores.unwrap_or("0").parse::<u64>().unwrap_or(0);

  Ok(HwInfo {
    chip_name,
    mac_model,
    memory_gb: mem_gb as u16,
    ecpu_cores: ecpu_cores as u8,
    mcpu_cores: mcpu_cores as u8,
    pcpu_cores: pcpu_cores as u8,
    ecpu_label: ecpu_label.into(),
    pcpu_label: pcpu_label.into(),
    gpu_cores: gpu_cores as u8,
  })
}

fn profiler_topology(s: &str, os_major: u32) -> (u64, u64, u64, &'static str, &'static str) {
  // macOS 27 reports total:Super:Performance:Efficiency; macOS 26 used
  // total:top:Efficiency:middle. Never interpret the new format as the old one.
  if os_major >= 27 {
    let parts = s.strip_prefix("proc ").unwrap_or("").split(':')
      .map(str::parse::<u64>).collect::<Result<Vec<_>, _>>();
    if let Ok(parts) = parts {
      match parts.as_slice() {
        [_, 0, p, e] => return (*e, 0, *p, "E", "P"),
        [_, top, p, 0] => return (*p, 0, *top, "P", "S"),
        [_, top, p, e] => return (*e, *p, *top, "E", "S"),
        _ => {}
      }
    }
  }
  let (lo, hi, middle) = parse_cpu_cores(s);
  (lo, 0, hi, if middle { "P" } else { "E" }, if middle { "S" } else { "P" })
}

fn is_pmgr_node(name: &str) -> bool {
  matches!(name, "pmgr" | "pmgr-child")
}

fn load_soc_info() -> WithError<SocInfo> {
  let hw = match hw_native() {
    Ok(hw) => hw,
    Err(_) => hw_from_profiler()?,
  };

  let mut info = SocInfo {
    chip_name: hw.chip_name,
    mac_model: hw.mac_model,
    memory_gb: hw.memory_gb,
    ecpu_cores: hw.ecpu_cores,
    mcpu_cores: hw.mcpu_cores,
    pcpu_cores: hw.pcpu_cores,
    ecpu_label: hw.ecpu_label,
    pcpu_label: hw.pcpu_label,
    gpu_cores: hw.gpu_cores,
    ..Default::default()
  };

  let cpu_scale = cpu_freq_scale(&info.chip_name);
  let gpu_scale: u32 = 1000 * 1000; // MHz

  // CPU/GPU frequencies always come from IOKit directly, regardless of how the
  // rest of the hardware descriptor above was sourced.
  for (entry, name) in IOServiceIterator::new("AppleARMIODevice")? {
    if is_pmgr_node(&name) {
      let item = cfio_get_props(entry, name)?;
      // 1) `strings /usr/bin/powermetrics | grep voltage-states` uses non-sram keys
      //    but their values are zero, so sram used here; it looks valid.
      // 2) sudo powermetrics --samplers cpu_power -i 1000 -n 1 | grep "active residency" | grep "Cluster"
      // M6 moved the tables to pmgr-child; pmgr is an empty stub. A later
      // stub must not replace a table already found on another node.
      if info.ecpu_freqs.is_empty() && let Some(f) = cpu_freqs(item, "voltage-states1-sram", true, cpu_scale) {
        info.ecpu_freqs = f;
      }
      if info.pcpu_freqs.is_empty() && let Some(f) = cpu_freqs(item, "voltage-states5-sram", false, cpu_scale) {
        info.pcpu_freqs = f;
      }

      if info.gpu_freqs.is_empty() && let Some((_, freqs)) = get_dvfs_mhz(item, "voltage-states9") {
        info.gpu_freqs = to_mhz(freqs, gpu_scale);
      }
      unsafe { CFRelease(item as _) }
    }
  }

  if info.ecpu_freqs.is_empty() || info.pcpu_freqs.is_empty() {
    return Err("No CPU frequencies found".into());
  }

  // M6 PACC0_MCPU and PACC0_PCPU expose the same 20 residency states of
  // the P complex. Use its table for both, as upstream macmon does.
  if info.mcpu_cores > 0 {
    info.mcpu_freqs = info.pcpu_freqs.clone();
  }

  Ok(info)
}

/// Load cached static SoC information for the current machine.
pub fn get_soc_info() -> WithError<SocInfo> {
  if let Some(info) = SOC_INFO_CACHE.get() {
    return Ok(info.clone());
  }

  let info = load_soc_info()?;
  let _ = SOC_INFO_CACHE.set(info.clone());
  Ok(info)
}

// MARK: IOReport

struct IOReportChannels {
  chan: CFMutableDictionaryRef,
  source: Option<CFDictionaryRef>,
  selected: Option<CFMutableArrayRef>,
}

impl Drop for IOReportChannels {
  fn drop(&mut self) {
    unsafe {
      CFRelease(self.chan.cast());
      if let Some(selected) = self.selected { CFRelease(selected.cast()); }
      if let Some(source) = self.source { CFRelease(source.cast()); }
    }
  }
}

fn clpc_energy_channel(
  driver: u64,
  id: u64,
  name: &str,
  unit: &str,
) -> CFMutableDictionary<CFString, CFType> {
  // IOReportTypes.h: simple integer, power category, one element; energy in nJ or mJ.
  let channel_type = 1i64 | (2 << 16) | (1 << 32);
  let unit = (3i64 << 56) | ((if unit == "mJ" { 124i64 } else { 118 }) << 32);
  let legend = CFArray::from_CFTypes(&[
    CFNumber::from(id as i64).into_CFType(),
    CFNumber::from(channel_type).into_CFType(),
    CFString::new(name).into_CFType(),
  ]);
  let info = CFDictionary::from_CFType_pairs(&[(
    CFString::new("IOReportChannelUnit"),
    CFNumber::from(unit),
  )]);
  let channel = CFDictionary::from_CFType_pairs(&[
    (CFString::new("DriverID"), CFNumber::from(driver as i64).into_CFType()),
    (CFString::new("DriverName"), CFString::new("AppleCLPC").into_CFType()),
    (CFString::new("IOReportGroupName"), CFString::new("CLPC").into_CFType()),
    (CFString::new("IOReportSubGroupName"), CFString::new("Energy Counters").into_CFType()),
    (CFString::new("IOReportChannelInfo"), info.into_CFType()),
    (CFString::new("LegendChannel"), legend.into_CFType()),
  ]);
  // IOReport's getters cache derived fields in the channel dictionary.
  CFMutableDictionary::from(&channel)
}

fn cfio_add_clpc_channels(selected: CFMutableArrayRef, filter: Option<ChannelFilterRef<'_>>) {
  let Some(os_version) = sysctl_str("kern.osproductversion") else { return };
  let Ok(services) = IOServiceIterator::new("AppleCLPC") else { return };
  for (entry, name) in services {
    let mut driver = 0;
    let result = unsafe { IORegistryEntryGetRegistryEntryID(entry.raw(), &mut driver) };
    let props = if result == 0 { cfio_get_props(entry, name).ok() } else { None };
    let Some(props) = props else { continue };
    let channels = cfdict_get_val(props, "CFBundleIdentifier").and_then(|value| {
      let bundle = unsafe { CFType::wrap_under_get_rule(value) }.downcast::<CFString>()?;
      crate::clpc::energy_channels(&bundle.to_string(), &os_version)
    });
    unsafe { CFRelease(props as _) };
    let Some(channels) = channels else { continue };

    cfio_append_clpc_channels(selected, driver, &channels, filter);
  }
}

fn cfio_append_clpc_channels(
  selected: CFMutableArrayRef,
  driver: u64,
  channels: &[(u64, &str, &str)],
  filter: Option<ChannelFilterRef<'_>>,
) {
  for &(id, name, unit) in channels {
    if filter.is_some_and(|f| !f("CLPC", "Energy Counters", name, unit)) {
      continue;
    }

    let exists = (0..unsafe { CFArrayGetCount(selected) }).any(|i| {
      let item = unsafe { CFArrayGetValueAtIndex(selected, i) } as CFDictionaryRef;
      // Alternate table indices can resolve to the same underlying CLPC counter.
      unsafe {
        IOReportChannelGetDriverID(item) == driver
          && IOReportChannelGetChannelID(item) as u32 == id as u32
      }
    });
    if !exists {
      let channel = clpc_energy_channel(driver, id, name, unit);
      unsafe { CFArrayAppendValue(selected, channel.as_CFTypeRef()) };
    }
  }
}

fn cfio_get_chan(filter: Option<ChannelFilterRef<'_>>, include_clpc: bool) -> WithError<IOReportChannels> {
  let all_channels = unsafe { IOReportCopyAllChannels(0, 0) };
  if all_channels.is_null() { return Err("Failed to copy IOReport channels".into()); }
  let channel_array = match channel_array(all_channels) {
    Ok(array) => array,
    Err(error) => {
      unsafe { CFRelease(all_channels.cast()) };
      return Err(error);
    }
  };
  let size = unsafe { CFDictionaryGetCount(all_channels) };
  let chan = unsafe { CFDictionaryCreateMutableCopy(kCFAllocatorDefault, size, all_channels) };
  if chan.is_null() {
    unsafe { CFRelease(all_channels.cast()) };
    return Err("Failed to copy IOReport channel dictionary".into());
  }

  let count = channel_array.len();
  let selected = unsafe { CFArrayCreateMutable(kCFAllocatorDefault, count, &kCFTypeArrayCallBacks) };
  if selected.is_null() {
    unsafe { CFRelease(chan.cast()); CFRelease(all_channels.cast()); }
    return Err("Failed to allocate IOReport channel array".into());
  }
  // Own every Create/Copy result before discovery of optional extra channels.
  let channels = IOReportChannels { chan, source: Some(all_channels), selected: Some(selected) };
  for raw in channel_array.iter() {
    let owner = unsafe { CFType::wrap_under_get_rule(*raw) }.downcast_into::<CFDictionary>()
      .ok_or("IOReport channel is not a dictionary")?;
    let item = owner.as_concrete_TypeRef();
    let group = cfio_get_group(item);
    let subgroup = cfio_get_subgroup(item);
    let channel = cfio_get_channel(item);
    let unit = from_cfstr(unsafe { IOReportChannelGetUnitLabel(item) }).trim().to_string();
    if filter.is_none_or(|f| f(&group, &subgroup, &channel, &unit)) {
      unsafe { CFArrayAppendValue(selected, item as _) };
    }
  }
  if include_clpc { cfio_add_clpc_channels(selected, filter); }
  let key = CFString::new("IOReportChannels");
  unsafe { CFDictionarySetValue(chan, key.as_CFTypeRef(), selected as _) };
  Ok(channels)
}

fn cfio_get_subs(chan: CFMutableDictionaryRef) -> WithError<(IOReportSubscriptionRef, CFMutableDictionaryRef)> {
  let mut subscribed = null_mut();
  let rs = unsafe { IOReportCreateSubscription(null(), chan, &mut subscribed, 0, null()) };
  if rs.is_null() || subscribed.is_null() {
    unsafe {
      if !subscribed.is_null() { CFRelease(subscribed.cast()); }
      if !rs.is_null() { CFRelease(rs.cast()); }
    }
    return Err("Failed to create subscription".into());
  }

  Ok((rs, subscribed))
}

/// IOReport subscription used to sample Apple Silicon power and residency counters.
pub struct IOReport {
  subs: IOReportSubscriptionRef,
  channels: IOReportChannels,
  prev: Option<(CFDictionary, std::time::Instant)>,
}

impl IOReport {
  fn from_filter(filter: Option<ChannelFilterRef<'_>>) -> WithError<Self> {
    let subscribe = |mut channels: IOReportChannels| -> WithError<Self> {
      let (subs, subscribed) = cfio_get_subs(channels.chan)?;
      unsafe { CFRelease(channels.chan.cast()) };
      channels.chan = subscribed;
      Ok(Self { subs, channels, prev: None })
    };
    match subscribe(cfio_get_chan(filter, true)?) {
      Ok(report) => Ok(report),
      // Optional energy counters must not prevent temperatures/activity from working.
      Err(_) => subscribe(cfio_get_chan(filter, false)?),
    }
  }

  /// Subscribe to IOReport channels by group and optional subgroup.
  pub fn new(channels: Vec<(&str, Option<&str>)>) -> WithError<Self> {
    let filter = |group: &str, subgroup: &str, _channel: &str, _unit: &str| {
      cfio_channel_matches(&channels, group, subgroup)
    };
    Self::from_filter(Some(&filter))
  }

  pub(crate) fn with_filter(filter: Option<ChannelFilter>) -> WithError<Self> {
    match filter {
      Some(filter) => Self::from_filter(Some(&filter)),
      None => Self::from_filter(None),
    }
  }

  /// Collect one delta sample over `duration` milliseconds.
  pub fn get_sample(&self, duration: u64) -> WithError<IOReportIterator> {
    let (first, _) = self.raw_sample()?;
    std::thread::sleep(Duration::from_millis(duration));
    let (last, _) = self.raw_sample()?;
    Self::delta(&first, &last)
  }

  fn raw_sample(&self) -> WithError<(CFDictionary, std::time::Instant)> {
    let sample = unsafe { take_sample(IOReportCreateSamples(self.subs, self.channels.chan, null()))? };
    Ok((sample, std::time::Instant::now()))
  }

  fn delta(first: &CFDictionary, last: &CFDictionary) -> WithError<IOReportIterator> {
    let raw = unsafe { IOReportCreateSamplesDelta(first.as_concrete_TypeRef(), last.as_concrete_TypeRef(), null()) };
    parse_sample(unsafe { take_sample(raw)? })
  }

  pub(crate) fn get_sample_interval(&mut self, duration: Duration) -> WithError<(IOReportIterator, Duration)> {
    let prev = match self.prev.take() {
      Some(sample) => sample,
      None => self.raw_sample()?,
    };
    std::thread::sleep(duration.saturating_sub(prev.1.elapsed()));
    let next = self.raw_sample()?;
    let elapsed = next.1.duration_since(prev.1).max(Duration::from_nanos(1));
    let sample = Self::delta(&prev.0, &next.0)?;
    self.prev = Some(next);
    Ok((sample, elapsed))
  }

  /// Collect multiple delta samples across one sampling window.
  pub fn get_samples(&mut self, duration: u64, count: usize) -> WithError<Vec<(IOReportIterator, u64)>> {
    let count = count.clamp(1, 32);
    let mut samples = Vec::with_capacity(count);
    let mut prev = match self.prev.take() {
      Some(sample) => sample,
      None => self.raw_sample()?,
    };
    let started_at = prev.1;
    for i in 1..=count {
      let target = Duration::from_millis(duration.saturating_mul(i as u64) / count as u64);
      std::thread::sleep(target.saturating_sub(started_at.elapsed()));
      let next = self.raw_sample()?;
      let sample = Self::delta(&prev.0, &next.0)?;
      let elapsed = next.1.duration_since(prev.1).as_millis() as u64;
      samples.push((sample, elapsed.max(1)));
      prev = next;
    }
    self.prev = Some(prev);
    Ok(samples)
  }
}

impl Drop for IOReport {
  fn drop(&mut self) {
    unsafe { CFRelease(self.subs as _) };
  }
}

// MARK: IOHID Bindings
// referenced from: https://github.com/freedomtan/sensors/blob/master/sensors/sensors.m

#[repr(C)]
struct IOHIDServiceClient(libc::c_void);

#[repr(C)]
struct IOHIDEventSystemClient(libc::c_void);

#[repr(C)]
struct IOHIDEvent(libc::c_void);

type IOHIDServiceClientRef = *const IOHIDServiceClient;
type IOHIDEventSystemClientRef = *const IOHIDEventSystemClient;
type IOHIDEventRef = *const IOHIDEvent;

const kHIDPage_AppleVendor: i32 = 0xff00;
const kHIDUsage_AppleVendor_TemperatureSensor: i32 = 0x0005;

const kIOHIDEventTypeTemperature: i64 = 15;
const kIOHIDEventTypePower: i64 = 25;

#[link(name = "IOKit", kind = "framework")]
#[rustfmt::skip]
unsafe extern "C" {
  fn IOHIDEventSystemClientCreate(allocator: CFAllocatorRef) -> IOHIDEventSystemClientRef;
  fn IOHIDEventSystemClientSetMatching(a: IOHIDEventSystemClientRef, b: CFDictionaryRef) -> i32;
  fn IOHIDEventSystemClientCopyServices(a: IOHIDEventSystemClientRef) -> CFArrayRef;
  fn IOHIDServiceClientCopyProperty(a: IOHIDServiceClientRef, b: CFStringRef) -> CFTypeRef;
  fn IOHIDServiceClientCopyEvent(a: IOHIDServiceClientRef, v0: i64, v1: i32, v2: i64) -> IOHIDEventRef;
  fn IOHIDEventGetFloatValue(event: IOHIDEventRef, field: i64) -> f64;
}

// MARK: IOHIDSensors

/// IOHID temperature sensor reader.
pub struct IOHIDSensors {
  matching: CFDictionary<CFString, CFNumber>,
  product: CFString,
}

impl IOHIDSensors {
  /// Create an IOHID temperature sensor reader.
  pub fn new() -> WithError<Self> {
    let matching = CFDictionary::from_CFType_pairs(&[
      (CFString::from_static_string("PrimaryUsagePage"), CFNumber::from(kHIDPage_AppleVendor)),
      (CFString::from_static_string("PrimaryUsage"), CFNumber::from(kHIDUsage_AppleVendor_TemperatureSensor)),
    ]);
    Ok(Self { matching, product: CFString::from_static_string("Product") })
  }

  /// Read temperature sensor values as `(sensor_name, celsius)`.
  pub fn get_metrics(&self) -> Vec<(String, f32)> {
    let mut items = Vec::new();
    unsafe {
      let raw = IOHIDEventSystemClientCreate(kCFAllocatorDefault);
      if raw.is_null() {
        return items;
      }
      // All Create/Copy results are owned, including on early returns and skipped sensors.
      let system = CFType::wrap_under_create_rule(raw.cast());
      let client = system.as_CFTypeRef().cast();
      IOHIDEventSystemClientSetMatching(client, self.matching.as_concrete_TypeRef());
      let raw = IOHIDEventSystemClientCopyServices(client);
      if raw.is_null() {
        return items;
      }
      let services: CFArray = CFArray::wrap_under_create_rule(raw);
      for entry in services.iter() {
        let service: IOHIDServiceClientRef = (*entry).cast();
        if service.is_null() {
          continue;
        }
        let raw = IOHIDServiceClientCopyProperty(service, self.product.as_concrete_TypeRef());
        if raw.is_null() {
          continue;
        }
        let property = CFType::wrap_under_create_rule(raw.cast());
        let Some(name) = property.downcast_into::<CFString>() else { continue };
        let raw = IOHIDServiceClientCopyEvent(service, kIOHIDEventTypeTemperature, 0, 0);
        if raw.is_null() {
          continue;
        }
        let event = CFType::wrap_under_create_rule(raw.cast());
        let temp = IOHIDEventGetFloatValue(event.as_CFTypeRef().cast(), kIOHIDEventTypeTemperature << 16);
        if temp > 0.0 && temp <= 150.0 {
          items.push((name.to_string(), temp as f32));
        }
      }
    }
    items.sort_by(|a, b| a.0.cmp(&b.0));
    items
  }
}

// MARK: SMC Bindings

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
  fn mach_task_self() -> u32;
  fn mach_port_deallocate(task: u32, name: u32) -> i32;
  fn IOServiceOpen(device: u32, a: u32, b: u32, c: *mut u32) -> i32;
  fn IOServiceClose(conn: u32) -> i32;
  fn IOConnectCallStructMethod(
    conn: u32,
    selector: u32,
    ival: *const c_void,
    isize: usize,
    oval: *mut c_void,
    osize: *mut usize,
  ) -> i32;
}

#[repr(C)]
#[derive(Debug, Default)]
/// SMC protocol version payload.
pub struct KeyDataVer {
  /// SMC protocol major version.
  pub major: u8,
  /// SMC protocol minor version.
  pub minor: u8,
  /// SMC protocol build number.
  pub build: u8,
  /// Reserved SMC protocol byte.
  pub reserved: u8,
  /// SMC protocol release number.
  pub release: u16,
}

#[repr(C)]
#[derive(Debug, Default)]
/// SMC power limit payload.
pub struct PLimitData {
  /// Payload version.
  pub version: u16,
  /// Payload length.
  pub length: u16,
  /// CPU power limit field.
  pub cpu_p_limit: u32,
  /// GPU power limit field.
  pub gpu_p_limit: u32,
  /// Memory power limit field.
  pub mem_p_limit: u32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
/// Metadata describing an SMC key value.
pub struct KeyInfo {
  /// Size of the SMC value payload in bytes.
  pub data_size: u32,
  /// FourCC data type for the SMC value.
  pub data_type: u32,
  /// SMC key attributes.
  pub data_attributes: u8,
}

#[repr(C)]
#[derive(Debug, Default)]
/// Raw SMC request/response payload.
pub struct KeyData {
  /// Four-byte SMC key encoded as a big-endian integer.
  pub key: u32,
  /// SMC protocol version data.
  pub vers: KeyDataVer,
  /// Power limit payload.
  pub p_limit_data: PLimitData,
  /// SMC key metadata.
  pub key_info: KeyInfo,
  /// SMC result byte.
  pub result: u8,
  /// SMC status byte.
  pub status: u8,
  /// 8-bit command/data field.
  pub data8: u8,
  /// 32-bit command/data field.
  pub data32: u32,
  /// Raw SMC value bytes.
  pub bytes: [u8; 32],
}

#[derive(Debug, Clone)]
/// Decoded SMC sensor value.
pub struct SensorVal {
  /// Four-byte SMC key name.
  pub name: String,
  /// FourCC unit/data type string.
  pub unit: String,
  /// Raw sensor bytes.
  pub data: Vec<u8>,
}

fn decode_smc_value(name: String, info: &KeyInfo, bytes: &[u8]) -> WithError<SensorVal> {
  let data = bytes.get(..info.data_size as usize).ok_or("SMC value exceeds response buffer")?;
  let unit = std::str::from_utf8(&info.data_type.to_be_bytes())?.to_owned();
  Ok(SensorVal { name, unit, data: data.to_vec() })
}

// MARK: SMC

#[allow(clippy::upper_case_acronyms)]
/// Apple System Management Controller connection.
pub struct SMC {
  conn: u32,
  keys: HashMap<u32, KeyInfo>,
}

impl SMC {
  /// Open an SMC connection.
  pub fn new() -> WithError<Self> {
    let mut conn = 0;

    for (device, name) in IOServiceIterator::new("AppleSMC")? {
      if name == "AppleSMCKeysEndpoint" {
        let rs = unsafe { IOServiceOpen(device.raw(), mach_task_self(), 0, &mut conn) };
        if rs != 0 {
          return Err(format!("IOServiceOpen: {}", rs).into());
        }
        break;
      }
    }

    if conn == 0 {
      return Err("AppleSMCKeysEndpoint unavailable".into());
    }
    Ok(Self { conn, keys: HashMap::new() })
  }

  fn read(&self, input: &KeyData) -> WithError<KeyData> {
    let ival = input as *const _ as _;
    let ilen = size_of::<KeyData>();
    let mut oval = KeyData::default();
    let mut olen = size_of::<KeyData>();

    let rs = unsafe {
      IOConnectCallStructMethod(self.conn, 2, ival, ilen, &mut oval as *mut _ as _, &mut olen)
    };

    if rs != 0 {
      // println!("{:?}", input);
      return Err(format!("IOConnectCallStructMethod: {}", rs).into());
    }

    if olen < std::mem::offset_of!(KeyData, result) + 1 {
      return Err("Truncated SMC response".into());
    }
    if oval.result == 132 {
      return Err("SMC key not found".into());
    }

    if oval.result != 0 {
      return Err(format!("SMC error: {}", oval.result).into());
    }

    if input.data8 == 5 {
      let size = input.key_info.data_size as usize;
      if size > oval.bytes.len() || olen < std::mem::offset_of!(KeyData, bytes) + size {
        return Err("Invalid SMC response payload length".into());
      }
    }
    Ok(oval)
  }

  fn parse_key(key: &str) -> WithError<u32> {
    if key.len() != 4 {
      return Err("SMC key must be 4 bytes long".into());
    }

    Ok(key.bytes().fold(0, |acc, x| (acc << 8) + x as u32))
  }

  fn read_key_info_by_id(&mut self, key: u32) -> WithError<KeyInfo> {
    if let Some(key_info) = self.keys.get(&key) {
      // println!("cache hit for {}", key);
      return Ok(*key_info);
    }

    let ival = KeyData { data8: 9, key, ..Default::default() };
    let oval = self.read(&ival)?;
    self.keys.insert(key, oval.key_info);
    Ok(oval.key_info)
  }

  /// Read an SMC key name by numeric index.
  pub fn key_by_index(&self, index: u32) -> WithError<String> {
    let ival = KeyData { data8: 8, data32: index, ..Default::default() };
    let oval = self.read(&ival)?;
    Ok(std::str::from_utf8(&oval.key.to_be_bytes())?.to_owned())
  }

  /// Read metadata for a four-byte SMC key.
  pub fn read_key_info(&mut self, key: &str) -> WithError<KeyInfo> {
    let key = Self::parse_key(key)?;
    self.read_key_info_by_id(key)
  }

  /// Read a raw SMC sensor value by four-byte key.
  pub fn read_val(&mut self, key: &str) -> WithError<SensorVal> {
    let name = key.to_string();
    let key = Self::parse_key(key)?;
    let key_info = self.read_key_info_by_id(key)?;
    let ival = KeyData { data8: 5, key, key_info, ..Default::default() };
    let oval = self.read(&ival)?;

    decode_smc_value(name, &key_info, &oval.bytes)
  }

  /// Read a four-byte float SMC value.
  pub fn read_float_val(&mut self, key: &str) -> WithError<f32> {
    const FLOAT_TYPE: u32 = 1718383648; // FourCC: "flt "

    let key_id = Self::parse_key(key)?;
    let key_info = self.read_key_info_by_id(key_id)?;
    if key_info.data_size != 4 || key_info.data_type != FLOAT_TYPE {
      return Err(
        format!(
          "SMC key '{}' is not a 4-byte float (size={}, type={})",
          key, key_info.data_size, key_info.data_type
        )
        .into(),
      );
    }

    let ival = KeyData { data8: 5, key: key_id, key_info, ..Default::default() };
    let oval = self.read(&ival)?;

    Ok(f32::from_le_bytes(oval.bytes[0..4].try_into().unwrap()))
  }

  /// Read the number of SMC keys exposed by the current machine.
  pub fn key_count(&mut self) -> WithError<u32> {
    let key = Self::parse_key("#KEY")?;
    let key_info = self.read_key_info_by_id(key)?;
    let ival = KeyData { data8: 5, key, key_info, ..Default::default() };
    let oval = self.read(&ival)?;
    Ok(u32::from_be_bytes(oval.bytes[0..4].try_into().unwrap()))
  }

  /// Enumerate all SMC key names.
  pub fn read_all_keys(&mut self) -> WithError<Vec<String>> {
    let count = self.key_count()?;

    let mut keys = Vec::new();
    for i in 0..count {
      match self.key_by_index(i) {
        Ok(key) => keys.push(key),
        Err(_) => continue,
      }
    }

    Ok(keys)
  }
}

impl Drop for SMC {
  fn drop(&mut self) {
    unsafe {
      IOServiceClose(self.conn);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn malformed_smc_metadata_returns_an_error() {
    let mut info = KeyInfo { data_type: u32::from_be_bytes(*b"flt "), data_size: 33, ..Default::default() };
    let bytes = [0; 32];
    assert!(decode_smc_value("F0Ac".into(), &info, &bytes).is_err());
    info.data_size = 4;
    info.data_type = 0xff00_ffff;
    assert!(decode_smc_value("F0Ac".into(), &info, &bytes).is_err());
  }

  #[test]
  fn used_ram_excludes_free_and_file_backed_pages_without_double_counting() {
    // Of 30 free pages, 10 are speculative and also in the 40 file-backed pages.
    for size in [4096, 16384] {
      assert_eq!(used_ram_bytes(100 * size, 30, 10, 40, size), 40 * size);
      assert_eq!(used_ram_bytes(100 * size, 0, 0, 0, size), 100 * size);
      assert_eq!(used_ram_bytes(100 * size, 100, 0, 20, size), 0);
    }
  }

  #[test]
  fn clpc_descriptors_preserve_driver_counter_ids_and_energy_units() {
    let driver = 0x1_0000_0428;
    let channels = crate::clpc::energy_channels("com.apple.driver.AppleT8152CLPC", "27.2").unwrap();
    for (id, name, unit) in channels {
      let channel = clpc_energy_channel(driver, id, name, unit);
      let item = channel.as_concrete_TypeRef();
      assert_eq!(unsafe { IOReportChannelGetDriverID(item) }, driver);
      assert_eq!(unsafe { IOReportChannelGetChannelID(item) }, id);
      assert_eq!(cfio_get_group(item), "CLPC");
      assert_eq!(cfio_get_subgroup(item), "Energy Counters");
      assert_eq!(cfio_get_channel(item), name);
      assert_eq!(from_cfstr(unsafe { IOReportChannelGetUnitLabel(item) }).trim(), unit);
    }
  }

  #[test]
  fn missing_or_malformed_ioreport_samples_return_errors() {
    assert!(unsafe { take_sample(null()) }.is_err());
    let wrong_type = CFString::new("unexpected");
    for key in ["another key", "IOReportChannels"] {
      let sample = CFDictionary::from_CFType_pairs(&[(CFString::new(key), wrong_type.clone())]);
      let before = sample.retain_count();
      assert!(parse_sample(sample.clone().into_untyped()).is_err());
      assert_eq!(sample.retain_count(), before);
    }
    let channels = CFArray::from_CFTypes(&[wrong_type]);
    let sample = CFDictionary::from_CFType_pairs(&[(CFString::new("IOReportChannels"), channels)]);
    assert!(parse_sample(sample.into_untyped()).is_err());
  }

  #[test]
  fn sample_metadata_follows_channels_after_omission_or_reordering() {
    let channel_ids: Vec<_> = [
      "com.apple.driver.AppleT8112CLPC",
      "com.apple.driver.AppleT6002CLPC",
      "com.apple.driver.AppleT8142CLPC",
    ]
    .into_iter()
    .flat_map(|bundle| crate::clpc::energy_channels(bundle, "27.0.1").unwrap())
    .collect();
    let requested: Vec<_> = channel_ids
      .iter()
      .map(|&(id, name, unit)| clpc_energy_channel(0x1_0000_0428, id, name, unit))
      .collect();
    for order in [vec![1], vec![2, 0, 1], vec![5, 3, 4], vec![8, 6, 7]] {
      let channels: Vec<_> = order.iter().map(|&i| requested[i].clone()).collect();
      let sample = CFDictionary::from_CFType_pairs(&[(
        CFString::new("IOReportChannels"),
        CFArray::from_CFTypes(&channels),
      )]);
      let actual: Vec<_> = parse_sample(sample.into_untyped()).unwrap()
        .map(|item| (item.channel, item.unit)).collect();
      let expected: Vec<_> =
        order.iter().map(|&i| (channel_ids[i].1.to_string(), "nJ".to_string())).collect();
      assert_eq!(actual, expected);
    }
  }

  #[test]
  fn clpc_subscription_deduplicates_counter_aliases_per_driver_and_respects_filter() {
    let driver = 0x1_0000_0428;
    let channels =
      crate::clpc::energy_channels("com.apple.driver.AppleT8142CLPC", "27.0.1").unwrap();
    let selected = unsafe { CFArrayCreateMutable(kCFAllocatorDefault, 0, &kCFTypeArrayCallBacks) };
    let alias = clpc_energy_channel(driver, 0x0000_0018_6a01_0933, "GPU Energy", "nJ");
    unsafe { CFArrayAppendValue(selected, alias.as_CFTypeRef()) };

    cfio_append_clpc_channels(selected, driver, &channels, None);
    cfio_append_clpc_channels(selected, driver, &channels, None);
    assert_eq!(unsafe { CFArrayGetCount(selected) }, 6);

    let gpu_only = |group: &str, subgroup: &str, name: &str, unit: &str| {
      group == "CLPC" && subgroup == "Energy Counters" && name == "GPU Energy" && unit == "nJ"
    };
    cfio_append_clpc_channels(selected, driver + 1, &channels, Some(&gpu_only));
    assert_eq!(unsafe { CFArrayGetCount(selected) }, 7);
    let item = unsafe { CFArrayGetValueAtIndex(selected, 6) } as CFDictionaryRef;
    assert_eq!(unsafe { IOReportChannelGetDriverID(item) }, driver + 1);
    assert_eq!(unsafe { IOReportChannelGetChannelID(item) }, 0x0000_0019_6a01_0933);

    unsafe { CFRelease(selected as _) };
  }



  #[test]
  fn handles_long_unicode_sensor_names() {
    let name = "Датчик температуры — ".repeat(40);
    let string = CFString::new(&name);
    assert_eq!(from_cfstr(string.as_concrete_TypeRef()), name);
    assert_eq!(from_cfstr(null()), "");
  }

  #[test]
  fn m_series_topologies_and_frequency_units() {
    for (chip, counts, expected, scale) in [
      ("Apple M1", vec![4, 4], (4, 0, 4, "E", "P"), 1_000_000),
      ("Apple M1 Pro", vec![8, 2], (2, 0, 8, "E", "P"), 1_000_000),
      ("Apple M1 Max", vec![8, 2], (2, 0, 8, "E", "P"), 1_000_000),
      ("Apple M1 Ultra", vec![16, 4], (4, 0, 16, "E", "P"), 1_000_000),
      ("Apple M2", vec![4, 4], (4, 0, 4, "E", "P"), 1_000_000),
      ("Apple M2 Pro", vec![8, 4], (4, 0, 8, "E", "P"), 1_000_000),
      ("Apple M2 Max", vec![8, 4], (4, 0, 8, "E", "P"), 1_000_000),
      ("Apple M2 Ultra", vec![16, 8], (8, 0, 16, "E", "P"), 1_000_000),
      ("Apple M3", vec![4, 4], (4, 0, 4, "E", "P"), 1_000_000),
      ("Apple M3 Pro", vec![6, 6], (6, 0, 6, "E", "P"), 1_000_000),
      ("Apple M3 Max", vec![12, 4], (4, 0, 12, "E", "P"), 1_000_000),
      ("Apple M3 Ultra", vec![24, 8], (8, 0, 24, "E", "P"), 1_000_000),
      ("Apple M4", vec![4, 6], (6, 0, 4, "E", "P"), 1_000),
      ("Apple M4 Pro", vec![10, 4], (4, 0, 10, "E", "P"), 1_000),
      ("Apple M4 Max", vec![12, 4], (4, 0, 12, "E", "P"), 1_000),
      ("Apple M5 Max", vec![6, 12], (12, 0, 6, "P", "S"), 1_000),
      ("Apple M6", vec![2, 4, 6], (6, 4, 2, "E", "S"), 1_000),
    ] {
      assert_eq!(topology_from_perflevels(&counts, &[], chip), Some(expected), "{chip}");
      assert_eq!(cpu_freq_scale(chip), scale, "{chip}");
    }
  }

  #[test]
  fn detects_m6_and_preserves_two_tier_macs() {
    let names = |values: &[&str]| values.iter().map(|s| Some(s.to_string())).collect::<Vec<_>>();
    assert_eq!(topology_from_perflevels(&[2, 4, 6], &names(&["Super", "Performance", "Efficiency"]), "Apple M6"),
      Some((6, 4, 2, "E", "S")));
    assert_eq!(topology_from_perflevels(&[4, 6], &names(&["Performance", "Efficiency"]), "Apple M4"),
      Some((6, 0, 4, "E", "P")));
    assert_eq!(topology_from_perflevels(&[4, 6], &names(&["Super", "Efficiency"]), "Apple M5"),
      Some((6, 0, 4, "E", "S")));
    assert_eq!(topology_from_perflevels(&[6, 12], &names(&["Super", "Performance"]), "Apple M5 Max"),
      Some((12, 0, 6, "P", "S")));
    assert_eq!(topology_from_perflevels(&[4, 4], &[], "Apple M1"), Some((4, 0, 4, "E", "P")));
    assert!(topology_from_perflevels(&[], &[], "Apple M6").is_none());
  }

  #[test]
  fn understands_profiler_format_changes() {
    assert_eq!(profiler_topology("proc 12:2:4:6", 27), (6, 4, 2, "E", "S"));
    assert_eq!(profiler_topology("proc 8:0:4:4", 27), (4, 0, 4, "E", "P"));
    assert_eq!(profiler_topology("proc 18:6:12:0", 27), (12, 0, 6, "P", "S"));
    assert_eq!(profiler_topology("proc 18:6:0:12", 26), (12, 0, 6, "P", "S"));
    assert_eq!(profiler_topology("proc 8:4:4:0", 26), (4, 0, 4, "E", "P"));
    assert_eq!(profiler_topology("proc 8:4:4", 15), (4, 0, 4, "E", "P"));
  }

  #[test]
  fn discovers_old_and_new_frequency_nodes() {
    assert!(is_pmgr_node("pmgr"));
    assert!(is_pmgr_node("pmgr-child"));
    assert!(!is_pmgr_node("pmgrtools"));
    // Actual M6 table entries, stored in kHz.
    assert_eq!(to_mhz(vec![972_000, 2_940_000], cpu_freq_scale("Apple M6")), vec![972, 2940]);
    assert_eq!(to_mhz(vec![600_000_000], cpu_freq_scale("Apple M1")), vec![600]);
  }

  #[test]
  fn parse_acc_clusters_values() {
    // Real acc-clusters bytes captured from M5 Max via ioreg
    #[rustfmt::skip]
    let data = [
      0x16, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
      0x17, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
      0x05, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    let (e, p) = parse_acc_clusters(&data).unwrap();
    // Second-highest type (1 = Performance) as ecpu, highest (2 = Super) as pcpu
    assert_eq!(e, "voltage-states23-sram");
    assert_eq!(p, "voltage-states5-sram");
    assert!(parse_acc_clusters(&[]).is_none());
    // Single cluster – need both ecpu and pcpu
    assert!(parse_acc_clusters(&[1, 0, 0, 0, 0, 0, 0, 0]).is_none());
  }

  #[test]
  fn parse_cpu_core_counts() {
    for (value, expected) in [
      ("proc 18:6:0:12", (12, 6, true)),
      ("proc 16:12:4:0", (4, 12, false)),
      ("proc 8:4:4:0", (4, 4, false)),
      ("proc 8:4:4", (4, 4, false)),
      ("", (0, 0, false)),
      ("garbage", (0, 0, false)),
      ("10:8:2", (0, 0, false)),
      ("proc 8", (0, 0, false)),
      ("proc 8:4", (0, 0, false)),
      ("proc 24:6:0:12:6", (0, 0, false)),
    ] {
      assert_eq!(parse_cpu_cores(value), expected, "{value}");
    }
  }

  #[test]
  fn converts_energy_using_the_exact_sample_duration() {
    let watts = watts_from_energy(1_000_000.0, "uJ", Duration::from_micros(250_500)).unwrap();
    assert!((watts - 3.992_016).abs() < 0.000_001);
  }

  #[test]
  fn cfio_channel_filter_semantics() {
    let group = [("Energy Model", None)];
    assert!(cfio_channel_matches(&group, "Energy Model", ""));
    assert!(cfio_channel_matches(&group, "Energy Model", "CPU Core Performance States"));
    assert!(!cfio_channel_matches(&group, "CPU Stats", "CPU Core Performance States"));

    let subgroup = [("CPU Stats", Some("CPU Core Performance States"))];
    assert!(cfio_channel_matches(&subgroup, "CPU Stats", "CPU Core Performance States"));
    assert!(!cfio_channel_matches(&subgroup, "CPU Stats", "CPU Performance States"));
    assert!(!cfio_channel_matches(&subgroup, "GPU Stats", "CPU Core Performance States"));

    assert!(cfio_channel_matches(&[], "CPU Stats", "CPU Core Performance States"));
    assert!(cfio_channel_matches(&[], "Energy Model", ""));
  }
}
