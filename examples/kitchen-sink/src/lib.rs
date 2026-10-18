//! The kitchen sink: a plugin that uses every part of the host API, as a reference to copy
//! from and as the loader's end-to-end test.
//!
//! In `start` it registers one of everything (settings of every kind, a hotkey, a command, a
//! subscription, a service). When the loader announces it as running, it runs `selftest.rs`,
//! which exercises the rest — settings, the registries, services, guarded calls, messages,
//! events, console lines and all three hook kinds — and logs one line per check plus a total:
//! `kitchen self-test: N passed, M failed`. `kitchen.selftest` runs it again from the console.
//!
//! When the overlay plugin is loaded it also uses the overlay service (`overlay.rs`): a panel
//! with one of every widget, a toast, a dialog and a present subscriber.

mod hooks;
mod overlay;
mod selftest;

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use uniloader_sdk::{
    export_plugin, topics, Dependency, Host, Plugin, PluginError, PluginRef, PluginState,
    PluginStateEvent, Setting,
};

/// The service this plugin provides, as its consumers would see it in a client crate.
#[repr(C)]
pub struct MathService {
    /// `a + b`.
    pub add: extern "C" fn(a: i32, b: i32) -> i32,
}

extern "C" fn add(a: i32, b: i32) -> i32 {
    a.wrapping_add(b)
}

static MATH: MathService = MathService { add };

/// Counters the callbacks bump, which the self-test reads back.
pub struct KitchenSink {
    setting_changes: AtomicU32,
    commands: AtomicU32,
    events: AtomicU32,
    hotkeys: AtomicU32,
    ran: AtomicBool,
    /// The overlay, when one is loaded.
    overlay: Option<uniloader_overlay::Overlay>,
}

impl Plugin for KitchenSink {
    const NAME: &'static str = "kitchen";
    const TITLE: &'static str = "Kitchen sink";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const AUTHORS: &'static str = "Bluscream";
    const DESCRIPTION: &'static str = "Exercises every host function and checks the results.";
    const DEPENDENCIES: &'static [Dependency] = &[
        Dependency::plugin("hello", ">=0.1").optional(),
        Dependency::plugin(uniloader_overlay::PLUGIN, ">=0.1").optional(),
    ];

    // Providing a service is unsafe: the table's layout is a promise the compiler cannot check.
    #[allow(unsafe_code)]
    fn start(host: Host) -> Result<Self, PluginError> {
        host.setting(&Setting::bool("enabled", "Enabled", true))?;
        host.setting(&Setting::int("level", "Level", 3, 0, 10).with_description("0 to 10."))?;
        host.setting(&Setting::float("scale", "Scale", 1.5, 0.1, 4.0))?;
        host.setting(&Setting::choice("mode", "Mode", "fast", &["fast", "safe"]))?;
        host.setting(&Setting::text("motto", "Motto", "everything").advanced())?;
        host.setting(&Setting::int("counter", "Counter", 0, 0, 0).transient())?;
        host.hotkey("poke", "Poke the kitchen sink", "ctrl+f8")?;
        // Bound by position, so it only fires from a reported press; the self-test reports one.
        host.hotkey("scan", "Scan code binding", "sc58")?;
        host.command("selftest", "Run the self-test again.", "")?;
        host.command("echo", "Count and print the arguments.", "<text>")?;
        host.subscribe(topics::PLUGIN_STATE)?;
        host.subscribe("kitchen.ping")?;
        // SAFETY: `MathService` is the `#[repr(C)]` layout `kitchen.math` v1 promises, and
        // `add` is thread safe.
        unsafe { host.provide("kitchen.math", 1, &MATH)? };
        Ok(KitchenSink {
            setting_changes: AtomicU32::new(0),
            commands: AtomicU32::new(0),
            events: AtomicU32::new(0),
            hotkeys: AtomicU32::new(0),
            ran: AtomicBool::new(false),
            overlay: overlay::attach(&host),
        })
    }

    fn on_hotkey(&self, host: &Host, action: &str) {
        self.hotkeys.fetch_add(1, Ordering::Relaxed);
        host.console_print(&format!("kitchen: {action} pressed"));
    }

    fn on_setting_changed(&self, _host: &Host, key: &str, value: &str) {
        log::debug!("{key} = {value}");
        self.setting_changes.fetch_add(1, Ordering::Relaxed);
    }

    fn on_command(&self, host: &Host, name: &str, args: &str) -> Result<(), PluginError> {
        self.commands.fetch_add(1, Ordering::Relaxed);
        match name {
            "selftest" => {
                selftest::run(self, host);
                Ok(())
            }
            "echo" => {
                host.console_print(args);
                Ok(())
            }
            other => Err(PluginError::from(format!("no command {other}"))),
        }
    }

    fn on_message(
        &self,
        _host: &Host,
        _from: PluginRef,
        topic: &str,
        payload: &[u8],
    ) -> Result<Option<Vec<u8>>, PluginError> {
        match topic {
            "echo" => Ok(Some(payload.to_vec())),
            _ => Err(PluginError::Unsupported),
        }
    }

    fn on_event(&self, host: &Host, _from: PluginRef, topic: &str, payload: &[u8]) {
        self.events.fetch_add(1, Ordering::Relaxed);
        if topic != topics::PLUGIN_STATE {
            return;
        }
        let Some(event) = PluginStateEvent::parse(payload) else {
            return;
        };
        log::info!("plugin {} is now {}", event.name, event.state.as_str());
        // The first moment every plugin is up: run the self-test once.
        let me = event.plugin == host.me() && event.state == PluginState::RUNNING;
        if me && !self.ran.swap(true, Ordering::Relaxed) {
            selftest::run(self, host);
        }
    }
}

export_plugin!(KitchenSink);
