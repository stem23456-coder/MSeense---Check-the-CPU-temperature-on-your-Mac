// ===== MSense settings: edit these constants before building =====
#[cfg(target_os = "macos")]
const DONATION_MENU_TITLE: &str = "♡ Donate to Developer…";
#[cfg(target_os = "macos")]
const DONATION_DIALOG_TITLE: &str = "Support MSense";
#[cfg(target_os = "macos")]
const DONATION_MESSAGE: &str =
    "If MSense is useful to you, please consider supporting the developer. Thank you!";
#[cfg(target_os = "macos")]
const DONATION_BTC_ADDRESS: &str = "bc1qsud09kj6pdng2cn7tzknajtzvmyar9hr29mkzr";
// Leave empty to hide the QR button. You can use an https:// URL or an absolute
// local path, for example: /Users/your-name/Pictures/msense-btc-qr.png
#[cfg(target_os = "macos")]
const DONATION_QR_URL_OR_PATH: &str = "http://qrcoder.ru/code/?bc1qsud09kj6pdng2cn7tzknajtzvmyar9hr29mkzr&4&0";

// 0 = automatic detection (recommended for a distributed build).
// Set a non-zero value only as a temporary workaround for one specific Mac.
#[cfg(target_os = "macos")]
const EFFICIENCY_CORE_COUNT_OVERRIDE: usize = 0;
#[cfg(target_os = "macos")]
const PERFORMANCE_CORE_COUNT_OVERRIDE: usize = 0;

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("msense-menu works only on macOS / Apple Silicon.");
}

#[cfg(target_os = "macos")]
#[path = "../login_item.rs"]
mod login_item;

#[cfg(target_os = "macos")]
mod app {
    use super::login_item::LoginItem;
    use super::{
        DONATION_BTC_ADDRESS, DONATION_DIALOG_TITLE, DONATION_MENU_TITLE, DONATION_MESSAGE,
        DONATION_QR_URL_OR_PATH, EFFICIENCY_CORE_COUNT_OVERRIDE,
        PERFORMANCE_CORE_COUNT_OVERRIDE,
    };
    use macmon::{Metrics, Sampler, SocInfo};
    use macmon::sources::{IOHIDSensors, SMC};
    use std::{collections::HashSet, process::Command, sync::{Arc, Mutex}, thread};
    use tray_icon::{
        Icon, TrayIcon, TrayIconBuilder, TrayIconEvent,
        menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
    };
    use winit::{
        application::ApplicationHandler,
        event_loop::{ActiveEventLoop, EventLoop},
        platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS},
    };

    #[derive(Debug, Default)]
    struct Thermals {
        efficiency: Vec<f32>,
        performance: Vec<f32>,
        other_cpu: Vec<f32>,
        gpu: Vec<f32>,
    }

    impl Thermals {
        fn cpu_max(&self) -> Option<f32> {
            self.efficiency
                .iter()
                .chain(self.performance.iter())
                .chain(self.other_cpu.iter())
                .copied()
                .filter(|value| valid_temp(*value))
                .max_by(|a, b| a.total_cmp(b))
        }

        fn gpu_average(&self) -> Option<f32> {
            average_temp(&self.gpu)
        }
    }

    #[derive(Debug)]
    struct Snapshot {
        metrics: Metrics,
        thermals: Thermals,
    }

    #[derive(Debug)]
    enum UserEvent {
        MetricsReady,
        Menu(MenuId),
        Fatal(String),
    }

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let soc = SocInfo::new()?;

        let mut builder = EventLoop::<UserEvent>::with_user_event();
        builder
            .with_activation_policy(ActivationPolicy::Accessory)
            .with_default_menu(false);
        let event_loop = builder.build()?;
        let proxy = event_loop.create_proxy();

        let sensor_chip = soc.chip_name.clone();
        let sensor_low_label = soc.ecpu_label.clone();
        let sensor_soc_efficiency_count = soc.ecpu_cores as usize;
        let sensor_soc_performance_count = soc.pcpu_cores as usize;
        let metrics_proxy = proxy.clone();
        // Only the most recent reading is useful when the menu event loop resumes.
        let latest = Arc::new(Mutex::new(None));
        let worker_latest = Arc::clone(&latest);
        thread::spawn(move || {
            let mut sampler = match Sampler::new() {
                Ok(sampler) => sampler,
                Err(error) => {
                    let _ = metrics_proxy.send_event(UserEvent::Fatal(error.to_string()));
                    return;
                }
            };
            let mut temperature_reader = TemperatureReader::new(&sensor_chip, &sensor_low_label);

            loop {
                match sampler.get_metrics(1000) {
                    Ok(metrics) => {
                        let efficiency_count = configured_core_count(
                            EFFICIENCY_CORE_COUNT_OVERRIDE,
                            sensor_soc_efficiency_count,
                            metrics.ecpu_cores.len(),
                        );
                        let performance_count = configured_core_count(
                            PERFORMANCE_CORE_COUNT_OVERRIDE,
                            sensor_soc_performance_count,
                            metrics.pcpu_cores.len(),
                        );
                        let snapshot = Snapshot {
                            metrics,
                            thermals: temperature_reader
                                .read(efficiency_count, performance_count),
                        };
                        let needs_wakeup = worker_latest.lock().unwrap().replace(snapshot).is_none();
                        if needs_wakeup && metrics_proxy.send_event(UserEvent::MetricsReady).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = metrics_proxy.send_event(UserEvent::Fatal(error.to_string()));
                        break;
                    }
                }
            }
        });

        let menu_proxy = proxy.clone();
        // Only menu selections are used. The default receiver stores every tray mouse event.
        TrayIconEvent::set_event_handler(Some(|_| {}));
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let _ = menu_proxy.send_event(UserEvent::Menu(event.id));
        }));

        let mut app = MenuBarApp::new(soc, latest);
        event_loop.run_app(&mut app)?;
        Ok(())
    }

    struct TemperatureReader {
        hid: Option<IOHIDSensors>,
        smc: Option<SmcTemperatureReader>,
    }

    impl TemperatureReader {
        fn new(chip_name: &str, low_label: &str) -> Self {
            Self {
                hid: IOHIDSensors::new().ok(),
                smc: SmcTemperatureReader::new(chip_name, low_label),
            }
        }

        fn read(&mut self, efficiency_count: usize, performance_count: usize) -> Thermals {
            let mut result = self
                .hid
                .as_ref()
                .map(read_hid_temperatures)
                .unwrap_or_default();

            if let Some(smc) = &mut self.smc {
                let fallback = smc.read();
                result.efficiency = choose_core_temperatures(
                    result.efficiency,
                    fallback.efficiency,
                    efficiency_count,
                );
                result.performance = choose_core_temperatures(
                    result.performance,
                    fallback.performance,
                    performance_count,
                );
                if result.gpu.is_empty() {
                    result.gpu = fallback.gpu;
                }
                result.other_cpu.extend(fallback.other_cpu);
            } else {
                result.efficiency =
                    fit_temperatures_to_cores(result.efficiency, efficiency_count);
                result.performance =
                    fit_temperatures_to_cores(result.performance, performance_count);
            }

            result
        }
    }

    struct SmcTemperatureReader {
        smc: SMC,
        efficiency_keys: Vec<String>,
        performance_keys: Vec<String>,
        other_cpu_keys: Vec<String>,
        gpu_keys: Vec<String>,
    }

    impl SmcTemperatureReader {
        fn new(chip_name: &str, low_label: &str) -> Option<Self> {
            let mut smc = SMC::new().ok()?;
            let available: HashSet<String> = smc.read_all_keys().ok()?.into_iter().collect();
            let (efficiency_candidates, performance_candidates, gpu_candidates) =
                smc_sensor_candidates(chip_name, low_label);

            let efficiency_keys = present_keys(&available, efficiency_candidates);
            let performance_keys = present_keys(&available, performance_candidates);
            let mut gpu_keys = present_keys(&available, gpu_candidates);

            let other_cpu_keys = if efficiency_keys.is_empty() && performance_keys.is_empty() {
                let mut keys: Vec<_> = available
                    .iter()
                    .filter(|key| {
                        key.starts_with("Tp")
                            || key.starts_with("Te")
                            || key.starts_with("Ts")
                    })
                    .cloned()
                    .collect();
                keys.sort();
                keys
            } else {
                Vec::new()
            };

            if gpu_keys.is_empty() {
                gpu_keys = available
                    .iter()
                    .filter(|key| key.starts_with("Tg"))
                    .cloned()
                    .collect();
                gpu_keys.sort();
            }

            Some(Self {
                smc,
                efficiency_keys,
                performance_keys,
                other_cpu_keys,
                gpu_keys,
            })
        }

        fn read(&mut self) -> Thermals {
            Thermals {
                efficiency: read_smc_values(&mut self.smc, &self.efficiency_keys),
                performance: read_smc_values(&mut self.smc, &self.performance_keys),
                other_cpu: read_smc_values(&mut self.smc, &self.other_cpu_keys),
                gpu: read_smc_values(&mut self.smc, &self.gpu_keys),
            }
        }
    }

    fn read_hid_temperatures(hid: &IOHIDSensors) -> Thermals {
        let mut efficiency = Vec::new();
        let mut performance = Vec::new();
        let mut gpu = Vec::new();

        for (name, value) in hid.get_metrics() {
            if !valid_temp(value) {
                continue;
            }
            let normalized = name.to_ascii_lowercase();
            if normalized.starts_with("eacc mtr temp sensor") {
                efficiency.push((name, value));
            } else if normalized.starts_with("pacc mtr temp sensor") {
                performance.push((name, value));
            } else if normalized.starts_with("gpu mtr temp sensor") {
                gpu.push((name, value));
            }
        }

        Thermals {
            efficiency: sorted_sensor_values(efficiency),
            performance: sorted_sensor_values(performance),
            other_cpu: Vec::new(),
            gpu: sorted_sensor_values(gpu),
        }
    }

    fn sorted_sensor_values(mut values: Vec<(String, f32)>) -> Vec<f32> {
        values.sort_by(|a, b| {
            sensor_number(&a.0)
                .cmp(&sensor_number(&b.0))
                .then_with(|| a.0.cmp(&b.0))
        });
        values.into_iter().map(|(_, value)| value).collect()
    }

    fn sensor_number(name: &str) -> Option<u32> {
        let digits_reversed: String = name
            .chars()
            .rev()
            .skip_while(|character| !character.is_ascii_digit())
            .take_while(|character| character.is_ascii_digit())
            .collect();
        if digits_reversed.is_empty() {
            None
        } else {
            digits_reversed.chars().rev().collect::<String>().parse().ok()
        }
    }

    fn choose_core_temperatures(
        primary: Vec<f32>,
        fallback: Vec<f32>,
        core_count: usize,
    ) -> Vec<f32> {
        let values = if core_count > 0 && primary.len() == core_count {
            primary
        } else if core_count > 0 && fallback.len() == core_count {
            fallback
        } else if !primary.is_empty() {
            primary
        } else {
            fallback
        };
        fit_temperatures_to_cores(values, core_count)
    }

    // Apple exposes thermal sensor channels, and their number is not guaranteed
    // to match the number of physical CPU cores. Normalize those channels to the
    // real topology: use the hottest channel when several cover one core, and
    // reuse the nearest cluster channel when one sensor covers multiple cores.
    fn fit_temperatures_to_cores(values: Vec<f32>, core_count: usize) -> Vec<f32> {
        if values.is_empty() || core_count == 0 || values.len() == core_count {
            return values;
        }

        if values.len() < core_count {
            return (0..core_count)
                .map(|index| values[index * values.len() / core_count])
                .collect();
        }

        (0..core_count)
            .filter_map(|index| {
                let start = index * values.len() / core_count;
                let end = ((index + 1) * values.len() / core_count).max(start + 1);
                values[start..end]
                    .iter()
                    .copied()
                    .filter(|value| valid_temp(*value))
                    .max_by(|a, b| a.total_cmp(b))
            })
            .collect()
    }

    fn present_keys(available: &HashSet<String>, candidates: &[&str]) -> Vec<String> {
        candidates
            .iter()
            .filter(|key| available.contains(**key))
            .map(|key| (*key).to_string())
            .collect()
    }

    fn read_smc_values(smc: &mut SMC, keys: &[String]) -> Vec<f32> {
        keys.iter()
            .filter_map(|key| smc.read_float_val(key).ok())
            .filter(|value| valid_temp(*value))
            .collect()
    }

    fn smc_sensor_candidates(
        chip_name: &str,
        low_label: &str,
    ) -> (&'static [&'static str], &'static [&'static str], &'static [&'static str]) {
        const M1_E: &[&str] = &["Tp09", "Tp0T"];
        const M1_P: &[&str] = &["Tp01", "Tp05", "Tp0D", "Tp0H", "Tp0L", "Tp0P", "Tp0X", "Tp0b"];
        const M1_G: &[&str] = &["Tg05", "Tg0D", "Tg0L", "Tg0T"];

        const M2_E: &[&str] = &["Tp1h", "Tp1t", "Tp1p", "Tp1l"];
        const M2_P: &[&str] = &["Tp01", "Tp05", "Tp09", "Tp0D", "Tp0X", "Tp0b", "Tp0f", "Tp0j"];
        const M2_G: &[&str] = &["Tg0f", "Tg0j"];

        const M3_E: &[&str] = &["Te05", "Te0L", "Te0P", "Te0S"];
        const M3_P: &[&str] = &[
            "Tf04", "Tf09", "Tf0A", "Tf0B", "Tf0D", "Tf0E",
            "Tf44", "Tf49", "Tf4A", "Tf4B", "Tf4D", "Tf4E",
        ];
        const M3_G: &[&str] = &["Tf14", "Tf18", "Tf19", "Tf1A", "Tf24", "Tf28", "Tf29", "Tf2A"];

        const M4_E: &[&str] = &["Te05", "Te0S", "Te09", "Te0H"];
        const M4_P: &[&str] = &["Tp01", "Tp05", "Tp09", "Tp0D", "Tp0V", "Tp0Y", "Tp0b", "Tp0e"];
        const M4_G: &[&str] = &["Tg0G", "Tg0H", "Tg1U", "Tg1k", "Tg0K", "Tg0L", "Tg0d", "Tg0e", "Tg0j", "Tg0k"];

        const M5_P: &[&str] = &[
            "Tp0O", "Tp0R", "Tp0U", "Tp0X", "Tp0a", "Tp0d",
            "Tp0g", "Tp0j", "Tp0m", "Tp0p", "Tp0u", "Tp0y",
        ];
        const M5_S: &[&str] = &["Tp00", "Tp04", "Tp08", "Tp0C", "Tp0G", "Tp0K"];
        const M5_G: &[&str] = &["Tg0U", "Tg0X", "Tg0d", "Tg0g", "Tg0j", "Tg1Y", "Tg1c", "Tg1g"];
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

    struct MenuBarApp {
        soc: SocInfo,
        latest: Arc<Mutex<Option<Snapshot>>>,
        login: Option<LoginItem>,
        tray: Option<TrayIcon>,
        efficiency_temp_items: Vec<MenuItem>,
        performance_temp_items: Vec<MenuItem>,
        gpu_temp_item: Option<MenuItem>,
        cpu_temp_item: Option<MenuItem>,
        middle_cluster_item: Option<MenuItem>,
        middle_core_items: Vec<MenuItem>,
        cpu_summary_item: Option<MenuItem>,
        efficiency_cluster_item: Option<MenuItem>,
        performance_cluster_item: Option<MenuItem>,
        efficiency_core_items: Vec<MenuItem>,
        performance_core_items: Vec<MenuItem>,
        gpu_summary_item: Option<MenuItem>,
        gpu_detail_item: Option<MenuItem>,
        ram_item: Option<MenuItem>,
        swap_item: Option<MenuItem>,
        cpu_power_item: Option<MenuItem>,
        gpu_power_item: Option<MenuItem>,
        ane_power_item: Option<MenuItem>,
        ram_power_item: Option<MenuItem>,
        gpu_ram_power_item: Option<MenuItem>,
        compute_power_item: Option<MenuItem>,
        system_power_item: Option<MenuItem>,
        fan_items: Vec<MenuItem>,
        donation_id: Option<MenuId>,
        quit_id: Option<MenuId>,
        last_status: Option<(i16, i16)>,
    }

    impl MenuBarApp {
        fn new(soc: SocInfo, latest: Arc<Mutex<Option<Snapshot>>>) -> Self {
            Self {
                soc,
                latest,
                login: None,
                tray: None,
                efficiency_temp_items: Vec::new(),
                performance_temp_items: Vec::new(),
                gpu_temp_item: None,
                cpu_temp_item: None,
                middle_cluster_item: None,
                middle_core_items: Vec::new(),
                cpu_summary_item: None,
                efficiency_cluster_item: None,
                performance_cluster_item: None,
                efficiency_core_items: Vec::new(),
                performance_core_items: Vec::new(),
                gpu_summary_item: None,
                gpu_detail_item: None,
                ram_item: None,
                swap_item: None,
                cpu_power_item: None,
                gpu_power_item: None,
                ane_power_item: None,
                ram_power_item: None,
                gpu_ram_power_item: None,
                compute_power_item: None,
                system_power_item: None,
                fan_items: Vec::new(),
                donation_id: None,
                quit_id: None,
                last_status: None,
            }
        }

        fn build_tray(&mut self, snapshot: &Snapshot) -> Result<(), Box<dyn std::error::Error>> {
            let menu = Menu::new();
            let efficiency_name = tier_name(&self.soc.ecpu_label);
            let performance_name = tier_name(&self.soc.pcpu_label);
            let efficiency_count = configured_core_count(
                EFFICIENCY_CORE_COUNT_OVERRIDE,
                self.soc.ecpu_cores as usize,
                snapshot.metrics.ecpu_cores.len(),
            );
            let performance_count = configured_core_count(
                PERFORMANCE_CORE_COUNT_OVERRIDE,
                self.soc.pcpu_cores as usize,
                snapshot.metrics.pcpu_cores.len(),
            );

            // macOS draws disabled NSMenuItems with a low-contrast gray color.
            // Keep information rows enabled for normal readable text; menu
            // events from them are intentionally ignored in user_event().
            menu.append(&MenuItem::new("🌡  Temperatures", true, None))?;
            let cpu_temp = MenuItem::new("CPU maximum: …", true, None);
            menu.append(&cpu_temp)?;
            self.cpu_temp_item = Some(cpu_temp);
            // Only show per-core rows when this chip has a known sensor mapping.
            // M6 currently exposes CPU sensors without a verified core association.
            for (name, count, values, items) in [
                (efficiency_name, efficiency_count, &snapshot.thermals.efficiency, &mut self.efficiency_temp_items),
                (performance_name, performance_count, &snapshot.thermals.performance, &mut self.performance_temp_items),
            ] {
                if values.is_empty() { continue; }
                for index in 0..count {
                    let item = MenuItem::new(format!("{name} Core {}: …", index + 1), true, None);
                    menu.append(&item)?;
                    items.push(item);
                }
            }
            let gpu_temp = MenuItem::new("GPU: …", true, None);
            menu.append(&gpu_temp)?;
            self.gpu_temp_item = Some(gpu_temp);

            menu.append(&PredefinedMenuItem::separator())?;
            menu.append(&MenuItem::new("Processor", true, None))?;
            let cpu_summary = MenuItem::new("CPU Total: …", true, None);
            menu.append(&cpu_summary)?;
            self.cpu_summary_item = Some(cpu_summary);
            for (name, count, cluster, items) in [
                (efficiency_name, efficiency_count, &mut self.efficiency_cluster_item, &mut self.efficiency_core_items),
                ("Performance", self.soc.mcpu_cores as usize, &mut self.middle_cluster_item, &mut self.middle_core_items),
                (performance_name, performance_count, &mut self.performance_cluster_item, &mut self.performance_core_items),
            ] {
                if count == 0 { continue; }
                let item = MenuItem::new(format!("{name} Cluster: …"), true, None);
                menu.append(&item)?;
                *cluster = Some(item);
                for index in 0..count {
                    let item = MenuItem::new(format!("{name} Core {}: …", index + 1), true, None);
                    menu.append(&item)?;
                    items.push(item);
                }
            }

            menu.append(&PredefinedMenuItem::separator())?;
            menu.append(&MenuItem::new("Graphics", true, None))?;
            let gpu_summary = MenuItem::new("GPU: …", true, None);
            let gpu_detail = MenuItem::new("GPU Details: …", true, None);
            menu.append(&gpu_summary)?;
            menu.append(&gpu_detail)?;
            self.gpu_summary_item = Some(gpu_summary);
            self.gpu_detail_item = Some(gpu_detail);

            menu.append(&PredefinedMenuItem::separator())?;
            menu.append(&MenuItem::new("Memory", true, None))?;
            let ram = MenuItem::new("RAM: …", true, None);
            let swap = MenuItem::new("Swap: …", true, None);
            menu.append(&ram)?;
            menu.append(&swap)?;
            self.ram_item = Some(ram);
            self.swap_item = Some(swap);

            menu.append(&PredefinedMenuItem::separator())?;
            menu.append(&MenuItem::new("Power", true, None))?;
            let cpu_power = MenuItem::new("CPU: …", true, None);
            let gpu_power = MenuItem::new("GPU: …", true, None);
            let ane_power = MenuItem::new("Neural Engine: …", true, None);
            let ram_power = MenuItem::new("DRAM: …", true, None);
            let gpu_ram_power = MenuItem::new("GPU SRAM: …", true, None);
            let compute_power = MenuItem::new("Compute Total: …", true, None);
            let system_power = MenuItem::new("System Total: …", true, None);
            for item in [
                &cpu_power,
                &gpu_power,
                &ane_power,
                &ram_power,
                &gpu_ram_power,
                &compute_power,
                &system_power,
            ] {
                menu.append(item)?;
            }
            self.cpu_power_item = Some(cpu_power);
            self.gpu_power_item = Some(gpu_power);
            self.ane_power_item = Some(ane_power);
            self.ram_power_item = Some(ram_power);
            self.gpu_ram_power_item = Some(gpu_ram_power);
            self.compute_power_item = Some(compute_power);
            self.system_power_item = Some(system_power);

            if !snapshot.metrics.fans.is_empty() {
                menu.append(&PredefinedMenuItem::separator())?;
                menu.append(&MenuItem::new("Cooling", true, None))?;
                for (index, _) in snapshot.metrics.fans.iter().enumerate() {
                    let item = MenuItem::new(format!("Fan {}: …", index + 1), true, None);
                    menu.append(&item)?;
                    self.fan_items.push(item);
                }
            }

            menu.append(&PredefinedMenuItem::separator())?;
            let donation = MenuItem::new(DONATION_MENU_TITLE, true, None);
            let quit = MenuItem::new("Quit MSense", true, None);
            let login = LoginItem::new();
            menu.append(&login.item)?;
            menu.append(&donation)?;
            menu.append(&quit)?;

            let cpu_max = cpu_max_temperature(snapshot);
            let gpu_average = gpu_temperature(snapshot);
            let tray = TrayIconBuilder::new()
                .with_icon(status_icon(cpu_max, gpu_average))
                .with_icon_as_template(true)
                .with_tooltip(format!("MSense — {}", self.soc.chip_name))
                .with_menu(Box::new(menu))
                .build()?;

            self.donation_id = Some(donation.id().clone());
            self.quit_id = Some(quit.id().clone());
            self.login = Some(login);
            self.tray = Some(tray);
            Ok(())
        }

        fn update(&mut self, snapshot: &Snapshot) {
            if let Some(login) = &mut self.login {
                login.refresh();
            }
            let metrics = &snapshot.metrics;
            let cpu_max = cpu_max_temperature(snapshot);
            let gpu_average = gpu_temperature(snapshot);
            let status = (rounded_temp(cpu_max), rounded_temp(gpu_average));

            if self.last_status != Some(status) {
                if let Some(tray) = &self.tray {
                    let _ = tray.set_icon_with_as_template(
                        Some(status_icon(cpu_max, gpu_average)),
                        true,
                    );
                }
                self.last_status = Some(status);
            }

            if let Some(tray) = &self.tray {
                let _ = tray.set_tooltip(Some(format!(
                    "{} — CPU max {} — GPU {}",
                    self.soc.chip_name,
                    display_temp(cpu_max),
                    display_temp(gpu_average),
                )));
            }

            let efficiency_name = tier_name(&self.soc.ecpu_label);
            let performance_name = tier_name(&self.soc.pcpu_label);

            for (index, item) in self.efficiency_temp_items.iter().enumerate() {
                item.set_text(format!(
                    "{efficiency_name} Core {}: {}",
                    index + 1,
                    display_temp(snapshot.thermals.efficiency.get(index).copied()),
                ));
            }
            for (index, item) in self.performance_temp_items.iter().enumerate() {
                item.set_text(format!(
                    "{performance_name} Core {}: {}",
                    index + 1,
                    display_temp(snapshot.thermals.performance.get(index).copied()),
                ));
            }
            if let Some(item) = &self.cpu_temp_item {
                item.set_text(format!("CPU maximum: {}", display_temp(cpu_max)));
            }
            if let Some(item) = &self.gpu_temp_item {
                item.set_text(format!("GPU: {}", display_temp(gpu_average)));
            }

            if let Some(item) = &self.cpu_summary_item {
                item.set_text(format!(
                    "CPU Total: {:.1}% active  ·  {:.1}% scaled  ·  {:.2} W",
                    metrics.cpu_active_ratio * 100.0,
                    metrics.cpu_scaled_ratio * 100.0,
                    metrics.cpu_power,
                ));
            }
            for (name, cluster, items, cores, freq, active, scaled) in [
                (efficiency_name, &self.efficiency_cluster_item, &self.efficiency_core_items,
                 &metrics.ecpu_cores, metrics.ecpu_freq_mhz, metrics.ecpu_active_ratio, metrics.ecpu_scaled_ratio),
                ("Performance", &self.middle_cluster_item, &self.middle_core_items,
                 &metrics.mcpu_cores, metrics.mcpu_freq_mhz, metrics.mcpu_active_ratio, metrics.mcpu_scaled_ratio),
                (performance_name, &self.performance_cluster_item, &self.performance_core_items,
                 &metrics.pcpu_cores, metrics.pcpu_freq_mhz, metrics.pcpu_active_ratio, metrics.pcpu_scaled_ratio),
            ] {
                if let Some(item) = cluster {
                    item.set_text(format!(
                        "{name} Cluster: {:.1}% active  ·  {:.1}% scaled  ·  {freq} MHz",
                        active * 100.0, scaled * 100.0,
                    ));
                }
                for (index, item) in items.iter().enumerate() {
                    let text = cores.get(index).map_or_else(
                        || format!("{name} Core {}: unavailable", index + 1),
                        |core| format!(
                            "{name} Core {}: {:.1}% active  ·  {:.1}% scaled  ·  {} MHz",
                            index + 1, core.active_ratio * 100.0, core.scaled_ratio * 100.0, core.freq_mhz,
                        ),
                    );
                    item.set_text(text);
                }
            }

            if let Some(item) = &self.gpu_summary_item {
                item.set_text(format!(
                    "GPU: {:.1}% active  ·  {:.1}% scaled  ·  {} MHz",
                    metrics.gpu_active_ratio * 100.0,
                    metrics.gpu_scaled_ratio * 100.0,
                    metrics.gpu_freq_mhz,
                ));
            }
            if let Some(item) = &self.gpu_detail_item {
                item.set_text(format!(
                    "Temperature: {}  ·  Power: {:.2} W  ·  {} cores",
                    display_temp(gpu_average),
                    metrics.gpu_power,
                    self.soc.gpu_cores,
                ));
            }

            let ram_percent = pct(metrics.memory.ram_usage, metrics.memory.ram_total);
            let swap_percent = pct(metrics.memory.swap_usage, metrics.memory.swap_total);
            if let Some(item) = &self.ram_item {
                item.set_text(format!(
                    "RAM: {:.2} / {:.2} GB  ({:.1}%)",
                    gb(metrics.memory.ram_usage),
                    gb(metrics.memory.ram_total),
                    ram_percent,
                ));
            }
            if let Some(item) = &self.swap_item {
                item.set_text(format!(
                    "Swap: {:.2} / {:.2} GB  ({:.1}%)",
                    gb(metrics.memory.swap_usage),
                    gb(metrics.memory.swap_total),
                    swap_percent,
                ));
            }

            set_power(&self.cpu_power_item, "CPU", metrics.cpu_power);
            set_power(&self.gpu_power_item, "GPU", metrics.gpu_power);
            set_power(&self.ane_power_item, "Neural Engine", metrics.ane_power);
            set_power(&self.ram_power_item, "DRAM", metrics.ram_power);
            set_power(&self.gpu_ram_power_item, "GPU SRAM", metrics.gpu_ram_power);
            set_power(&self.compute_power_item, "Compute Total", metrics.all_power);
            if let Some(item) = &self.system_power_item {
                if metrics.sys_power > 0.0 {
                    item.set_text(format!("System Total: {:.2} W", metrics.sys_power));
                } else {
                    item.set_text("System Total: unavailable");
                }
            }

            for (item, fan) in self.fan_items.iter().zip(metrics.fans.iter()) {
                match fan.max_rpm {
                    Some(max_rpm) => item.set_text(format!(
                        "{}: {} RPM  ·  max {} RPM",
                        fan.name, fan.rpm, max_rpm,
                    )),
                    None => item.set_text(format!("{}: {} RPM", fan.name, fan.rpm)),
                }
            }
        }
    }

    impl ApplicationHandler<UserEvent> for MenuBarApp {
        fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}

        fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
            match event {
                UserEvent::MetricsReady => {
                    let snapshot = self.latest.lock().unwrap().take();
                    let Some(snapshot) = snapshot else { return };
                    if self.tray.is_none() {
                        if let Err(error) = self.build_tray(&snapshot) {
                            eprintln!("MSense menu error: {error}");
                            event_loop.exit();
                            return;
                        }
                    }
                    self.update(&snapshot);
                }
                UserEvent::Menu(id) => {
                    if let Some(login) = &self.login {
                        if login.item.id() == &id {
                            login.toggle();
                            return;
                        }
                    }
                    if self.donation_id.as_ref() == Some(&id) {
                        show_donation_dialog();
                    } else if self.quit_id.as_ref() == Some(&id) {
                        event_loop.exit();
                    }
                }
                UserEvent::Fatal(message) => {
                    eprintln!("MSense sampler error: {message}");
                    if let Some(tray) = &self.tray {
                        let _ = tray.set_tooltip(Some(format!("MSense error: {message}")));
                    } else {
                        event_loop.exit();
                    }
                }
            }
        }

        fn window_event(
            &mut self,
            _event_loop: &ActiveEventLoop,
            _window_id: winit::window::WindowId,
            _event: winit::event::WindowEvent,
        ) {
        }
    }

    fn configured_core_count(override_count: usize, soc_count: usize, sampled_count: usize) -> usize {
        if override_count > 0 {
            override_count
        } else {
            // Static hardware info protects against samples that omit idle cores;
            // live per-core metrics protect against an incomplete hardware record.
            // Temperature sensor counts are intentionally never used here.
            soc_count.max(sampled_count)
        }
    }

    fn show_donation_dialog() {
        thread::spawn(|| {
            const SCRIPT: &str = r#"
on run argv
    set dialogTitle to item 1 of argv
    set donationMessage to item 2 of argv
    set btcAddress to item 3 of argv
    set qrTarget to item 4 of argv
    set dialogText to donationMessage & return & return & "Bitcoin (BTC):" & return & btcAddress

    if qrTarget is "" then
        set dialogResult to display dialog dialogText with title dialogTitle buttons {"Close", "Copy BTC"} default button "Copy BTC" with icon note
    else
        set dialogResult to display dialog dialogText with title dialogTitle buttons {"Close", "Open QR", "Copy BTC"} default button "Copy BTC" with icon note
    end if

    set chosenButton to button returned of dialogResult
    if chosenButton is "Copy BTC" then
        set the clipboard to btcAddress
        display notification "BTC address copied to clipboard" with title dialogTitle
    else if chosenButton is "Open QR" then
        do shell script "/usr/bin/open " & quoted form of qrTarget
    end if
end run
"#;

            let status = Command::new("/usr/bin/osascript")
                .arg("-e")
                .arg(SCRIPT)
                .arg(DONATION_DIALOG_TITLE)
                .arg(DONATION_MESSAGE)
                .arg(DONATION_BTC_ADDRESS)
                .arg(DONATION_QR_URL_OR_PATH)
                .status();

            if let Err(error) = status {
                eprintln!("MSense donation dialog error: {error}");
            }
        });
    }

    fn set_power(item: &Option<MenuItem>, label: &str, watts: f32) {
        if let Some(item) = item {
            item.set_text(format!("{label}: {watts:.2} W"));
        }
    }

    fn tier_name(label: &str) -> &'static str {
        match label {
            "E" => "Efficiency",
            "P" => "Performance",
            "S" => "Super",
            _ => "CPU",
        }
    }

    fn cpu_max_temperature(snapshot: &Snapshot) -> Option<f32> {
        snapshot
            .thermals
            .cpu_max()
            .or_else(|| valid_temp(snapshot.metrics.temp.cpu_temp_avg).then_some(snapshot.metrics.temp.cpu_temp_avg))
    }

    fn gpu_temperature(snapshot: &Snapshot) -> Option<f32> {
        snapshot
            .thermals
            .gpu_average()
            .or_else(|| valid_temp(snapshot.metrics.temp.gpu_temp_avg).then_some(snapshot.metrics.temp.gpu_temp_avg))
    }

    fn average_temp(values: &[f32]) -> Option<f32> {
        let valid: Vec<_> = values
            .iter()
            .copied()
            .filter(|value| valid_temp(*value))
            .collect();
        if valid.is_empty() {
            None
        } else {
            Some(valid.iter().sum::<f32>() / valid.len() as f32)
        }
    }

    fn valid_temp(value: f32) -> bool {
        value.is_finite() && value > 0.0 && value <= 150.0
    }

    fn display_temp(value: Option<f32>) -> String {
        value
            .filter(|value| valid_temp(*value))
            .map_or_else(|| "—".to_string(), |value| format!("{value:.0}°C"))
    }

    fn rounded_temp(value: Option<f32>) -> i16 {
        value
            .filter(|value| valid_temp(*value))
            .map_or(-1, |value| value.round() as i16)
    }

    // Match macOS Activity Monitor: decimal gigabytes, not binary gibibytes.
    fn gb(bytes: u64) -> f64 {
        bytes as f64 / 1_000_000_000.0
    }

    fn pct(used: u64, total: u64) -> f64 {
        if total == 0 {
            0.0
        } else {
            used as f64 * 100.0 / total as f64
        }
    }

    // The status item is a template bitmap instead of text. This keeps the two
    // temperatures truly stacked and lets macOS recolor them for light/dark mode.
    fn status_icon(cpu: Option<f32>, gpu: Option<f32>) -> Icon {
        const HEIGHT: u32 = 36;
        const SCALE: u32 = 2;
        const TEXT_X: u32 = 18;

        let top = compact_temp(cpu);
        let bottom = compact_temp(gpu);
        let text_width = text_width(&top, SCALE).max(text_width(&bottom, SCALE));
        let width = TEXT_X + text_width + 2;
        let mut rgba = vec![0_u8; (width * HEIGHT * 4) as usize];

        draw_thermometer(&mut rgba, width, HEIGHT);
        draw_text(&mut rgba, width, HEIGHT, TEXT_X, 1, SCALE, &top);
        draw_text(&mut rgba, width, HEIGHT, TEXT_X, 19, SCALE, &bottom);

        Icon::from_rgba(rgba, width, HEIGHT).expect("generated status icon is valid")
    }

    fn compact_temp(value: Option<f32>) -> String {
        value
            .filter(|value| valid_temp(*value))
            .map_or_else(|| "--°C".to_string(), |value| format!("{}°C", value.round() as i16))
    }

    fn text_width(text: &str, scale: u32) -> u32 {
        let units: u32 = text
            .chars()
            .map(|character| glyph(character).0 as u32 + 1)
            .sum();
        units.saturating_sub(1) * scale
    }

    fn draw_text(
        rgba: &mut [u8],
        image_width: u32,
        image_height: u32,
        x: u32,
        y: u32,
        scale: u32,
        text: &str,
    ) {
        let mut cursor = x;
        for character in text.chars() {
            let (glyph_width, rows) = glyph(character);
            for (row_index, row) in rows.iter().enumerate() {
                for column in 0..glyph_width {
                    let mask = 1_u8 << (glyph_width - 1 - column);
                    if *row & mask != 0 {
                        fill_rect(
                            rgba,
                            image_width,
                            image_height,
                            cursor + column as u32 * scale,
                            y + row_index as u32 * scale,
                            scale,
                            scale,
                            true,
                        );
                    }
                }
            }
            cursor += (glyph_width as u32 + 1) * scale;
        }
    }

    fn glyph(character: char) -> (u8, [u8; 7]) {
        match character {
            '0' => (5, [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110]),
            '1' => (5, [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110]),
            '2' => (5, [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111]),
            '3' => (5, [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110]),
            '4' => (5, [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010]),
            '5' => (5, [0b11111, 0b10000, 0b10000, 0b11110, 0b00001, 0b00001, 0b11110]),
            '6' => (5, [0b01110, 0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110]),
            '7' => (5, [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000]),
            '8' => (5, [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110]),
            '9' => (5, [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110]),
            'C' => (5, [0b01111, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b01111]),
            '°' => (3, [0b111, 0b101, 0b111, 0, 0, 0, 0]),
            '-' => (3, [0, 0, 0, 0b111, 0, 0, 0]),
            _ => (3, [0; 7]),
        }
    }

    fn draw_thermometer(rgba: &mut [u8], width: u32, height: u32) {
        fill_rect(rgba, width, height, 3, 3, 9, 23, true);
        fill_rect(rgba, width, height, 5, 5, 5, 19, false);
        fill_circle(rgba, width, height, 7, 27, 6, true);
        fill_circle(rgba, width, height, 7, 27, 4, false);
        fill_rect(rgba, width, height, 6, 12, 3, 16, true);
        fill_circle(rgba, width, height, 7, 27, 3, true);
    }

    fn fill_circle(
        rgba: &mut [u8],
        width: u32,
        height: u32,
        center_x: i32,
        center_y: i32,
        radius: i32,
        on: bool,
    ) {
        for y in (center_y - radius)..=(center_y + radius) {
            for x in (center_x - radius)..=(center_x + radius) {
                let dx = x - center_x;
                let dy = y - center_y;
                if dx * dx + dy * dy <= radius * radius && x >= 0 && y >= 0 {
                    set_pixel(rgba, width, height, x as u32, y as u32, on);
                }
            }
        }
    }

    fn fill_rect(
        rgba: &mut [u8],
        width: u32,
        height: u32,
        x: u32,
        y: u32,
        rect_width: u32,
        rect_height: u32,
        on: bool,
    ) {
        for pixel_y in y..y.saturating_add(rect_height) {
            for pixel_x in x..x.saturating_add(rect_width) {
                set_pixel(rgba, width, height, pixel_x, pixel_y, on);
            }
        }
    }

    fn set_pixel(
        rgba: &mut [u8],
        width: u32,
        height: u32,
        x: u32,
        y: u32,
        on: bool,
    ) {
        if x >= width || y >= height {
            return;
        }
        let index = ((y * width + x) * 4) as usize;
        rgba[index] = 255;
        rgba[index + 1] = 255;
        rgba[index + 2] = 255;
        rgba[index + 3] = if on { 255 } else { 0 };
    }
}

#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    app::run()
}
