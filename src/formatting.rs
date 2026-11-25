/// Binary memory units, matching the installed RAM capacity (24 GiB, etc.).
pub fn gib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0 * 1024.0)
}

pub fn power(watts: Option<f32>) -> String {
    watts.filter(|w| w.is_finite() && *w >= 0.0)
        .map_or_else(|| "unavailable".to_string(), |w| format!("{w:.2} W"))
}

pub fn activity(ratio: f32) -> String {
    if (0.0..=1.0).contains(&ratio) {
        format!("{:.1}%", ratio * 100.0)
    } else {
        "N/A".to_string()
    }
}

pub fn frequency(mhz: u32) -> String {
    if mhz == 0 { "N/A".to_string() } else { format!("{mhz} MHz") }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_residency_is_distinct_from_idle() {
        assert_eq!(activity(f32::NAN), "N/A");
        assert_eq!(activity(0.0), "0.0%");
        assert_eq!(activity(0.5), "50.0%");
        assert_eq!(frequency(0), "N/A");
        assert_eq!(frequency(1200), "1200 MHz");
    }

    #[test]
    fn installed_24_gib_is_not_25_decimal_gb() {
        assert_eq!(format!("{:.2}", gib(25_769_803_776)), "24.00");
        assert_eq!(gib(12_884_901_888), 12.0);
    }
    #[test]
    fn missing_power_is_distinct_from_idle_zero() {
        assert_eq!(power(None), "unavailable");
        assert_eq!(power(Some(0.0)), "0.00 W");
        assert_eq!(power(Some(12.5)), "12.50 W");
        assert_eq!(power(Some(f32::NAN)), "unavailable");
        assert_eq!(power(Some(-1.0)), "unavailable");
    }
}
