//! The smallest useful uniloader plugin: a setting, a hotkey, a console command and a
//! broadcast subscription. Build it as a `cdylib` and drop `hello.dll` into `plugins/`.

use std::sync::atomic::{AtomicU32, Ordering};

use uniloader_sdk::{export_plugin, Host, Plugin, PluginError, PluginRef, Setting};

/// Greets on a hotkey and counts how often it did.
pub struct Hello {
    greetings: AtomicU32,
}

impl Hello {
    fn greet(&self, host: &Host, name: &str) {
        let greeting = host.get("greeting").unwrap_or_default();
        let n = self.greetings.fetch_add(1, Ordering::Relaxed) + 1;
        host.console_print(&format!("{greeting}, {name}! (#{n})"));
    }
}

impl Plugin for Hello {
    const NAME: &'static str = "hello";
    const TITLE: &'static str = "Hello";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const AUTHORS: &'static str = "Bluscream";
    const DESCRIPTION: &'static str = "Greets on a hotkey.";

    fn start(host: Host) -> Result<Self, PluginError> {
        host.setting(
            &Setting::text("greeting", "Greeting", "Hello")
                .with_description("What the greet hotkey and command print."),
        )?;
        host.hotkey("greet", "Print the greeting", "f9")?;
        host.command("greet", "Print the greeting.", "[name]")?;
        host.subscribe("hello.ping")?;
        log::info!("hello {} started", Self::VERSION);
        Ok(Hello {
            greetings: AtomicU32::new(0),
        })
    }

    fn on_hotkey(&self, host: &Host, _action: &str) {
        self.greet(host, "player");
    }

    fn on_command(&self, host: &Host, _name: &str, args: &str) -> Result<(), PluginError> {
        self.greet(host, if args.is_empty() { "console" } else { args });
        Ok(())
    }

    fn on_event(&self, host: &Host, _from: PluginRef, _topic: &str, payload: &[u8]) {
        self.greet(host, &String::from_utf8_lossy(payload));
    }
}

export_plugin!(Hello);
