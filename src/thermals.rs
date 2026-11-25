use macmon::sources::{IOHIDSensors, SMC};

#[derive(Debug, Default)]
pub struct ThermalSnapshot {
    pub cpu: Vec<(String, f32)>,
    pub gpu: Vec<(String, f32)>,
}

pub fn valid_temp(value: f32) -> bool {
    value.is_finite() && value > 0.0 && value <= 150.0
}

pub fn hottest(sensors: &[(String, f32)]) -> Option<f32> {
    sensors.iter().map(|(_, value)| *value).reduce(f32::max)
}

pub fn average(sensors: &[(String, f32)]) -> Option<f32> {
    (!sensors.is_empty())
        .then(|| sensors.iter().map(|(_, value)| value).sum::<f32>() / sensors.len() as f32)
}

pub struct TemperatureReader {
    hid: Option<IOHIDSensors>,
    smc: Option<SMC>,
    cpu_keys: Vec<String>,
    gpu_keys: Vec<String>,
}

impl TemperatureReader {
    pub fn new(chip: &str, low_label: &str) -> Self {
        let mut smc = SMC::new().ok();
        let mut cpu_keys = Vec::new();
        let mut gpu_keys = Vec::new();
        let (low, high, gpu) = smc_sensor_candidates(chip, low_label);
        if let Some(reader) = &mut smc {
            let available = reader.read_all_keys().unwrap_or_default();
            for key in &available {
                let name = key.as_str();
                if low.contains(&name) || high.contains(&name) {
                    cpu_keys.push(key.clone());
                } else if gpu.contains(&name) {
                    gpu_keys.push(key.clone());
                }
            }
            if cpu_keys.is_empty() {
                cpu_keys.extend(
                    available
                        .iter()
                        .filter(|key| key.starts_with("Te") || key.starts_with("Tp"))
                        .cloned(),
                );
            }
            if gpu_keys.is_empty() {
                gpu_keys.extend(
                    available
                        .iter()
                        .filter(|key| key.starts_with("Tg"))
                        .cloned(),
                );
            }
        }
        cpu_keys.sort_unstable();
        gpu_keys.sort_unstable();
        Self {
            hid: IOHIDSensors::new().ok(),
            smc,
            cpu_keys,
            gpu_keys,
        }
    }

    pub fn read(&mut self) -> ThermalSnapshot {
        let mut out = ThermalSnapshot::default();
        if let Some(hid) = &self.hid {
            for (name, value) in hid.get_metrics() {
                if !valid_temp(value) {
                    continue;
                }
                let label = name.to_ascii_lowercase();
                if label.starts_with("eacc mtr temp sensor")
                    || label.starts_with("pacc mtr temp sensor")
                {
                    out.cpu.push((name, value));
                } else if label.starts_with("gpu mtr temp sensor") {
                    out.gpu.push((name, value));
                }
            }
        }
        if let Some(smc) = &mut self.smc {
            for (readings, keys) in [
                (&mut out.cpu, &self.cpu_keys),
                (&mut out.gpu, &self.gpu_keys),
            ] {
                if readings.is_empty() {
                    for key in keys {
                        let Ok(value) = smc.read_float_val(key) else {
                            continue;
                        };
                        if valid_temp(value) {
                            readings.push((key.clone(), value));
                        }
                    }
                }
            }
        }
        out.cpu.sort_by(|a, b| a.0.cmp(&b.0));
        out.gpu.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

fn smc_sensor_candidates(
    chip_name: &str,
    low_label: &str,
) -> (
    &'static [&'static str],
    &'static [&'static str],
    &'static [&'static str],
) {
    const M1_E: &[&str] = &["Tp09", "Tp0T"];
    const M1_P: &[&str] = &[
        "Tp01", "Tp05", "Tp0D", "Tp0H", "Tp0L", "Tp0P", "Tp0X", "Tp0b",
    ];
    const M1_G: &[&str] = &["Tg05", "Tg0D", "Tg0L", "Tg0T"];

    const M2_E: &[&str] = &["Tp1h", "Tp1t", "Tp1p", "Tp1l"];
    const M2_P: &[&str] = &[
        "Tp01", "Tp05", "Tp09", "Tp0D", "Tp0X", "Tp0b", "Tp0f", "Tp0j",
    ];
    const M2_G: &[&str] = &["Tg0f", "Tg0j"];

    const M3_E: &[&str] = &["Te05", "Te0L", "Te0P", "Te0S"];
    const M3_P: &[&str] = &[
        "Tf04", "Tf09", "Tf0A", "Tf0B", "Tf0D", "Tf0E", "Tf44", "Tf49", "Tf4A", "Tf4B", "Tf4D",
        "Tf4E",
    ];
    const M3_G: &[&str] = &[
        "Tf14", "Tf18", "Tf19", "Tf1A", "Tf24", "Tf28", "Tf29", "Tf2A",
    ];

    const M4_E: &[&str] = &["Te05", "Te0S", "Te09", "Te0H"];
    const M4_P: &[&str] = &[
        "Tp01", "Tp05", "Tp09", "Tp0D", "Tp0V", "Tp0Y", "Tp0b", "Tp0e",
    ];
    const M4_G: &[&str] = &[
        "Tg0G", "Tg0H", "Tg1U", "Tg1k", "Tg0K", "Tg0L", "Tg0d", "Tg0e", "Tg0j", "Tg0k",
    ];

    const M5_P: &[&str] = &[
        "Tp0O", "Tp0R", "Tp0U", "Tp0X", "Tp0a", "Tp0d", "Tp0g", "Tp0j", "Tp0m", "Tp0p", "Tp0u",
        "Tp0y",
    ];
    const M5_S: &[&str] = &["Tp00", "Tp04", "Tp08", "Tp0C", "Tp0G", "Tp0K"];
    const M5_G: &[&str] = &[
        "Tg0U", "Tg0X", "Tg0d", "Tg0g", "Tg0j", "Tg1Y", "Tg1c", "Tg1g",
    ];
    const NONE: &[&str] = &[];

    if chip_name.contains("M1") {
        (M1_E, M1_P, M1_G)
    } else if chip_name.contains("M2") {
        (M2_E, M2_P, M2_G)
    } else if chip_name.contains("M3") {
        (M3_E, M3_P, M3_G)
    } else if chip_name.contains("M4") {
        (M4_E, M4_P, M4_G)
    } else if chip_name.contains("M5") && low_label == "P" {
        (M5_P, M5_S, M5_G)
    } else if chip_name.contains("M5") {
        (NONE, M5_S, M5_G)
    } else {
        (NONE, NONE, NONE)
    }
}
