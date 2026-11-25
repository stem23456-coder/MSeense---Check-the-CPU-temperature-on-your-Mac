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
#[path = "../formatting.rs"]
mod formatting;

#[cfg(target_os = "macos")]
#[path = "../thermals.rs"]
mod thermals;

#[cfg(target_os = "macos")]
mod app {
    use super::formatting::{activity, frequency, gib, power};
    use super::login_item::LoginItem;
    use super::{
        DONATION_BTC_ADDRESS, DONATION_DIALOG_TITLE, DONATION_MENU_TITLE, DONATION_MESSAGE,
        DONATION_QR_URL_OR_PATH, EFFICIENCY_CORE_COUNT_OVERRIDE,
        PERFORMANCE_CORE_COUNT_OVERRIDE,
    };
    use macmon::{Metrics, Sampler, SocInfo};
    use super::thermals::{average, hottest, valid_temp, TemperatureReader, ThermalSnapshot};
    use std::{process::Command, sync::{Arc, Mutex}, thread};
    use tray_icon::{
        Icon, TrayIcon, TrayIconBuilder, TrayIconEvent,
        menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
    };
    use winit::{
        application::ApplicationHandler,
        event_loop::{ActiveEventLoop, EventLoop},
        platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS},
    };

    #[derive(Debug)]
    struct Snapshot {
        metrics: Metrics,
        thermals: ThermalSnapshot,
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
                        let snapshot = Snapshot { metrics, thermals: temperature_reader.read() };
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

    struct MenuBarApp {
        soc: SocInfo,
        latest: Arc<Mutex<Option<Snapshot>>>,
        login: Option<LoginItem>,
        tray: Option<TrayIcon>,
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
        fan_item: Option<MenuItem>,
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
                fan_item: None,
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

            menu.append(&PredefinedMenuItem::separator())?;
            let cooling = MenuItem::new("Cooling: …", true, None);
            menu.append(&cooling)?;
            self.fan_item = Some(cooling);

            menu.append(&PredefinedMenuItem::separator())?;
            let donation = MenuItem::new(DONATION_MENU_TITLE, true, None);
            let quit = MenuItem::new("Quit MSense", true, None);
            let login = LoginItem::new();
            menu.append(&login.item)?;
            menu.append(&donation)?;
            menu.append(&quit)?;

            let cpu_max = hottest(&snapshot.thermals.cpu);
            let gpu_average = average(&snapshot.thermals.gpu);
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
            let cpu_max = hottest(&snapshot.thermals.cpu);
            let gpu_average = average(&snapshot.thermals.gpu);
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

            if let Some(item) = &self.cpu_temp_item {
                item.set_text(format!("CPU maximum: {}", display_temp(cpu_max)));
            }
            if let Some(item) = &self.gpu_temp_item {
                item.set_text(format!("GPU: {}", display_temp(gpu_average)));
            }

            if let Some(item) = &self.cpu_summary_item {
                item.set_text(format!(
                    "CPU Total: {} active  ·  {} scaled  ·  {}",
                    activity(metrics.cpu_active_ratio),
                    activity(metrics.cpu_scaled_ratio),
                    power(metrics.cpu_power),
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
                        "{name} Cluster: {} active  ·  {} scaled  ·  {}",
                        activity(active), activity(scaled), frequency(freq),
                    ));
                }
                // The row numbers represent a complete ordered tier. A missing
                // channel could otherwise shift every following value by one.
                let cores = if cores.len() == items.len() { cores.as_slice() } else { &[] };
                for (index, item) in items.iter().enumerate() {
                    let text = cores.get(index).map_or_else(
                        || format!("{name} Core {}: unavailable", index + 1),
                        |core| format!(
                            "{name} Core {}: {} active  ·  {} scaled  ·  {}",
                            index + 1, activity(core.active_ratio), activity(core.scaled_ratio), frequency(core.freq_mhz),
                        ),
                    );
                    item.set_text(text);
                }
            }

            if let Some(item) = &self.gpu_summary_item {
                item.set_text(format!(
                    "GPU: {} active  ·  {} scaled  ·  {}",
                    activity(metrics.gpu_active_ratio),
                    activity(metrics.gpu_scaled_ratio),
                    frequency(metrics.gpu_freq_mhz),
                ));
            }
            if let Some(item) = &self.gpu_detail_item {
                item.set_text(format!(
                    "Temperature: {}  ·  Power: {}  ·  {} cores",
                    display_temp(gpu_average),
                    power(metrics.gpu_power),
                    self.soc.gpu_cores,
                ));
            }

            let ram_percent = pct(metrics.memory.ram_usage, metrics.memory.ram_total);
            let swap_percent = pct(metrics.memory.swap_usage, metrics.memory.swap_total);
            if let Some(item) = &self.ram_item {
                item.set_text(format!(
                    "RAM: {:.2} / {:.2} GiB  ({:.1}%)",
                    gib(metrics.memory.ram_usage),
                    gib(metrics.memory.ram_total),
                    ram_percent,
                ));
            }
            if let Some(item) = &self.swap_item {
                item.set_text(format!(
                    "Swap: {:.2} / {:.2} GiB  ({:.1}%)",
                    gib(metrics.memory.swap_usage),
                    gib(metrics.memory.swap_total),
                    swap_percent,
                ));
            }

            set_power(&self.cpu_power_item, "CPU", metrics.cpu_power);
            set_power(&self.gpu_power_item, "GPU", metrics.gpu_power);
            set_power(&self.ane_power_item, "Neural Engine", metrics.ane_power);
            set_power(&self.ram_power_item, "DRAM", metrics.ram_power);
            set_power(&self.gpu_ram_power_item, "GPU SRAM", metrics.gpu_ram_power);
            set_power(&self.compute_power_item, "Compute Total", metrics.all_power);
            set_power(&self.system_power_item, "System Total", metrics.sys_power);

            if let Some(item) = &self.fan_item {
                item.set_text(cooling_text(&metrics.fans));
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

    fn cooling_text(fans: &[macmon::FanMetric]) -> String {
        if fans.is_empty() {
            return "Cooling: unavailable".into();
        }
        fans.iter().map(|fan| match fan.max_rpm {
            Some(max) => format!("{}: {} RPM (max {max})", fan.name, fan.rpm),
            None => format!("{}: {} RPM", fan.name, fan.rpm),
        }).collect::<Vec<_>>().join("  ·  ")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn cooling_tracks_only_current_fans_including_stopped_fans() {
            let samples = [
                macmon::FanMetric { name: "fan0".into(), rpm: 0, max_rpm: None },
                macmon::FanMetric { name: "fan1".into(), rpm: 1300, max_rpm: Some(4200) },
            ];
            assert_eq!(cooling_text(&[]), "Cooling: unavailable");
            assert_eq!(cooling_text(&samples), "fan0: 0 RPM  ·  fan1: 1300 RPM (max 4200)");
            assert_eq!(cooling_text(&samples[1..]), "fan1: 1300 RPM (max 4200)");
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

    fn set_power(item: &Option<MenuItem>, label: &str, watts: Option<f32>) {
        if let Some(item) = item {
            item.set_text(format!("{label}: {}", power(watts)));
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
