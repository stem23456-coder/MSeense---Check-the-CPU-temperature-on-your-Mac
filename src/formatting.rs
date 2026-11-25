/// Binary memory units, matching the installed RAM capacity (24 GiB, etc.).
pub fn gib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0 * 1024.0)
}

pub fn power(watts: Option<f32>) -> String {
    watts.filter(|w| w.is_finite() && *w >= 0.0)
        .map_or_else(|| "unavailable".to_string(), |w| format!("{w:.2} W"))
}

#[cfg(test)]
mod tests {
    use super::*;
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
