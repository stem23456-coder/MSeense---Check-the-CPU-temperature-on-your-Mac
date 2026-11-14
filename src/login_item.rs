use objc2::{available, rc::Retained};
use objc2_foundation::{NSBundle, NSUserDefaults, ns_string};
use objc2_service_management::{SMAppService, SMAppServiceStatus as Status};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use tray_icon::menu::CheckMenuItem;

pub struct LoginItem {
    pub item: CheckMenuItem,
    service: Option<Retained<SMAppService>>,
    last_checked: Instant,
}

impl LoginItem {
    pub fn new() -> Self {
        let bundle = PathBuf::from(NSBundle::mainBundle().bundlePath().to_string());
        let installed = bundle.extension().is_some_and(|ext| ext == "app")
            && (bundle.starts_with("/Applications")
                || std::env::home_dir()
                    .is_some_and(|home| bundle.starts_with(home.join("Applications"))));
        let (service, title) = if !installed {
            (None, "Launch at Login (move MSense to Applications)")
        } else if available!(macos = 13.0) {
            // SMAppService is available from macOS 13. Use this installed bundle as the login item.
            (
                Some(unsafe { SMAppService::mainAppService() }),
                "Launch at Login",
            )
        } else {
            (None, "Launch at Login (requires macOS 13)")
        };
        let item = CheckMenuItem::new(title, service.is_some(), false, None);
        let login = Self {
            item,
            service,
            last_checked: Instant::now(),
        };
        if let Some(service) = &login.service {
            let defaults = NSUserDefaults::standardUserDefaults();
            let initialized = ns_string!("MSenseLoginItemInitialized");
            if !defaults.boolForKey(initialized) {
                let status = unsafe { service.status() };
                if status == Status::Enabled
                    || status == Status::RequiresApproval
                    || login.register(service)
                {
                    // After initial setup, respect changes in both the menu and System Settings.
                    defaults.setBool_forKey(true, initialized);
                }
            }
            login.show_status(service);
        }
        login
    }

    fn register(&self, service: &SMAppService) -> bool {
        match unsafe { service.registerAndReturnError() } {
            Ok(()) => true,
            Err(err) => {
                eprintln!("MSense could not enable Launch at Login: {err}");
                false
            }
        }
    }

    fn show_status(&self, service: &SMAppService) {
        let status = unsafe { service.status() };
        self.item.set_checked(status == Status::Enabled);
        self.item.set_text(if status == Status::RequiresApproval {
            "Launch at Login (approve in System Settings…)"
        } else {
            "Launch at Login"
        });
    }

    pub fn refresh(&mut self) {
        if self.last_checked.elapsed() >= Duration::from_secs(15) {
            if let Some(service) = &self.service {
                self.show_status(service);
            }
            self.last_checked = Instant::now();
        }
    }

    pub fn toggle(&self) {
        let Some(service) = &self.service else { return };
        let status = unsafe { service.status() };
        let succeeded = match status {
            Status::Enabled => match unsafe { service.unregisterAndReturnError() } {
                Ok(()) => true,
                Err(err) => {
                    eprintln!("MSense could not disable Launch at Login: {err}");
                    false
                }
            },
            Status::RequiresApproval => {
                unsafe { SMAppService::openSystemSettingsLoginItems() };
                true
            }
            _ => self.register(service),
        };
        if succeeded {
            NSUserDefaults::standardUserDefaults()
                .setBool_forKey(true, ns_string!("MSenseLoginItemInitialized"));
        }
        self.show_status(service);
        if !succeeded && unsafe { service.status() } != Status::RequiresApproval {
            self.item
                .set_text("Launch at Login (change failed; click to retry)");
        }
    }
}
