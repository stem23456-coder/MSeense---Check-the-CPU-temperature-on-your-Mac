#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("msense работает только на macOS / Apple Silicon.");
}

#[cfg(target_os = "macos")]
#[path = "../formatting.rs"]
mod formatting;

#[cfg(target_os = "macos")]
#[path = "../thermals.rs"]
mod thermals;

#[cfg(target_os = "macos")]
mod app {
    use super::formatting::{activity, frequency, gib, power};
    use macmon::{Metrics, Sampler, SocInfo};
    use super::thermals::{average, hottest, TemperatureReader, ThermalSnapshot};
    use std::env;
    use std::io::{self, Write};

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
        let mut raw_temps = TemperatureReader::new(&soc.chip_name, &soc.ecpu_label);

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
        println!("MSense  {}  ({})", soc.chip_name, soc.mac_model);
        println!("────────────────────────────────────────────────────────────────────────────────────");
        println!(
            "CPU {:>6}   hottest {}   avg {}   power {:>8}",
            activity(m.cpu_active_ratio),
            optional_temp(hottest(&temps.cpu)),
            optional_temp(average(&temps.cpu)),
            power(m.cpu_power)
        );
        println!(
            "GPU {:>6}   {:>8}   hottest {}   avg {}   power {:>8}   cores {}",
            activity(m.gpu_active_ratio),
            frequency(m.gpu_freq_mhz),
            optional_temp(hottest(&temps.gpu)),
            optional_temp(average(&temps.gpu)),
            power(m.gpu_power),
            soc.gpu_cores
        );
        println!(
            "POWER chip(CPU+GPU+ANE) {:>8}   system-estimate {}",
            power(m.all_power),
            power(m.sys_power)
        );
        println!(
            "      CPU {}   GPU {}   ANE {}   DRAM {}   GPU-SRAM {}",
            power(m.cpu_power), power(m.gpu_power), power(m.ane_power), power(m.ram_power), power(m.gpu_ram_power)
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
            "{}-tier avg: {:>8} / {:>6}    {}-tier avg: {:>8} / {:>6}",
            soc.ecpu_label,
            frequency(m.ecpu_freq_mhz),
            activity(m.ecpu_active_ratio),
            soc.pcpu_label,
            frequency(m.pcpu_freq_mhz),
            activity(m.pcpu_active_ratio),
        );

        println!("────────────────────────────────────────────────────────────────────────────────────");
        if soc.mcpu_cores > 0 {
            println!("P-tier avg: {:>8} / {:>6}", frequency(m.mcpu_freq_mhz), activity(m.mcpu_active_ratio));
        }
        println!("CORE    TYPE   FREQ       LOAD(active)   CAPACITY");

        for (label, cores, expected) in [
            (soc.ecpu_label.as_str(), &m.ecpu_cores, soc.ecpu_cores),
            ("P", &m.mcpu_cores, soc.mcpu_cores),
            (soc.pcpu_label.as_str(), &m.pcpu_cores, soc.pcpu_cores),
        ] {
            if expected > 0 && cores.len() != usize::from(expected) {
                println!("{label}-tier core readings unavailable ({} of {expected} channels)", cores.len());
                continue;
            }
            for (i, c) in cores.iter().enumerate() {
                println!(
                    "#{:<2}     {:>2}    {:>8}    {:>7}         {:>7}",
                    i + 1, label, frequency(c.freq_mhz), activity(c.active_ratio), activity(c.scaled_ratio),
                );
            }
        }

        println!("RAW THERMAL SENSORS");
        for (label, sensors) in [("CPU", &temps.cpu), ("GPU", &temps.gpu)] {
            if sensors.is_empty() {
                println!("  {label} sensors unavailable");
            }
            for (name, value) in sensors {
                println!("  {label}  {name}: {}", temp(*value));
            }
        }

        println!("Temperature colors: <90°C green, 90–99.9°C orange, 100°C+ red. Values are bold.");
        println!("System power is macOS/SMC system estimate; external monitor wall-power is not included reliably.");
        println!("Ctrl-C — выход");

        io::stdout().flush()?;
        Ok(())
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

    fn pct(used: u64, total: u64) -> f64 {
        if total == 0 { 0.0 } else { used as f64 * 100.0 / total as f64 }
    }
}

#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    app::run()
}
