//! The overlay: an in-game console and settings editor, and the `overlay` service other
//! plugins draw their panels, toasts and dialogs through.
//!
//! It is a plugin rather than part of the loader on purpose. Drawing inside a game means
//! hooking its presentation, and that is different for every graphics API and risky in every
//! one of them; a game that does not want an overlay should not carry one, and someone who
//! wants a different one should be able to swap it. The loader only offers what any overlay
//! needs and nothing that depends on a renderer: the console scrollback, the registries, key
//! presses and keyboard capture.
//!
//! What is here:
//!
//! - **Rendering**: `Direct3D` 11 through a universal DXGI hook (`win::hooks`). `Present` and
//!   `ResizeBuffers` live in one class vtable per DXGI implementation, so patching it through a
//!   throwaway swapchain covers every swapchain the game makes, before or after the plugin
//!   loads, whether the game proxies `dxgi.dll` or not. `Direct3D` 12, Vulkan and `OpenGL` games
//!   get no overlay yet; see the README for the plan.
//! - **UI**: egui, with the layer split so it never reaches a consumer (`ui`). Consumers get
//!   a C ABI ([`uniloader_overlay::OverlayApi`]) and fill panels one widget at a time.
//! - **Input**: the game window is subclassed while the overlay is up (`win::wnd`); key
//!   presses are reported to the loader so scan-code hotkeys fire, and the keyboard is
//!   claimed while a window has it, so typing does not trigger other plugins' hotkeys.

// Only the Windows renderer drives the UI; elsewhere it is built for the unit tests alone.
#![cfg_attr(
    not(windows),
    allow(dead_code, reason = "driven by the Windows renderer only")
)]

mod cards;
mod inject;
mod layout;
mod notices;
mod registry;
mod service;
mod shared;
mod ui;
#[cfg(windows)]
mod win;

use std::sync::OnceLock;

use uniloader_sdk::{
    export_plugin, topics, Host, Plugin, PluginError, PluginRef, PluginState, PluginStateEvent,
    Setting,
};

/// The loader, for code that runs outside a plugin callback: the render thread and the
/// window procedure.
static HOST: OnceLock<Host> = OnceLock::new();

/// The loader as seen by this plugin, once it has started.
pub(crate) fn host() -> Option<&'static Host> {
    HOST.get()
}

/// The plugin instance. Everything it needs lives in statics, because the render thread
/// and the window procedure reach it without a `&self`.
pub struct OverlayPlugin;

impl Plugin for OverlayPlugin {
    const NAME: &'static str = "overlay";
    const TITLE: &'static str = "Overlay";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const AUTHORS: &'static str = "Bluscream";
    const DESCRIPTION: &'static str =
        "In-game console and settings editor, and the overlay service plugins draw through.";
    const PROVIDES: &'static [&'static str] = &[uniloader_overlay::SERVICE];

    // Providing the service is unsafe: the table layout is a promise the compiler cannot check.
    #[allow(unsafe_code)]
    fn start(host: Host) -> Result<Self, PluginError> {
        // The key under Escape by position, because its name differs per layout; and a
        // chord for layouts where that key is a dead key.
        host.hotkey("console", "Show the in-game console", "sc29, ctrl+shift+c")?;
        host.hotkey("settings", "Show the settings editor", "f11")?;
        host.hotkey(
            "mouse",
            "Give the mouse back to the game, or take it",
            "ctrl+shift+m",
        )?;
        host.command(
            "toggle",
            "Open or close the console or the settings editor, or flip who has the mouse.",
            "console|settings|mouse",
        )?;
        host.command(
            "input",
            "Type, press a key or click in the overlay, as if from the keyboard and mouse.",
            inject::USAGE,
        )?;
        host.setting(
            &Setting::bool("drawing", "Drawing in this game", false)
                .transient()
                .advanced()
                .with_description(
                    "Set once the overlay has a swapchain it can draw on. A game's own UI \
                     reads it to open this console rather than its own.",
                ),
        )?;
        host.setting(
            &Setting::bool("cards_elsewhere", "A game's own UI shows the cards", false)
                .transient()
                .advanced()
                .with_description(
                    "Set by a game's own UI that shows the plugins' showcase cards its own \
                     way, from the overlay.card messages; the overlay then draws none.",
                ),
        )?;
        host.subscribe(topics::PLUGIN_STATE)?;
        // SAFETY: `service::TABLE` is the `OverlayApi` layout of this version, and every
        // function in it is thread safe.
        let provided = unsafe {
            host.provide(
                uniloader_overlay::SERVICE,
                uniloader_overlay::VERSION,
                &service::TABLE,
            )
        };
        // A game's own UI may provide the service already (tsw_ui): its panels go there, and
        // this plugin is the console and settings editor that open everywhere.
        if let Err(e) = provided {
            log::info!("the overlay service is provided by another plugin ({e}); console and settings only");
        }
        let _ = HOST.set(host);
        layout::load(&host);
        #[cfg(windows)]
        win::install(&host);
        #[cfg(not(windows))]
        log::warn!("no renderer on this platform; the service works but nothing is drawn");
        Ok(OverlayPlugin)
    }

    fn stop(&self, _host: &Host, _reason: uniloader_sdk::StopReason) {
        #[cfg(windows)]
        win::uninstall();
        layout::save();
    }

    fn on_command(&self, _host: &Host, name: &str, args: &str) -> Result<(), PluginError> {
        match (name, args.trim()) {
            ("toggle", what @ ("console" | "settings" | "mouse")) => {
                toggle(what);
                Ok(())
            }
            ("toggle", other) => Err(PluginError::from(format!(
                "toggle what? {other:?} is not console, settings or mouse"
            ))),
            ("input", args) => {
                let input = inject::parse(args).map_err(PluginError::from)?;
                #[cfg(windows)]
                return win::inject(&input).map_err(PluginError::from);
                #[cfg(not(windows))]
                {
                    let _ = input;
                    Err(PluginError::Unsupported)
                }
            }
            _ => Err(PluginError::Unsupported),
        }
    }

    fn on_hotkey(&self, _host: &Host, action: &str) {
        toggle(action);
    }

    fn on_setting_changed(&self, _host: &Host, key: &str, value: &str) {
        if key.rsplit('.').next() == Some("cards_elsewhere") {
            cards::set_elsewhere(value == "true");
        }
    }

    fn on_event(&self, _host: &Host, _from: PluginRef, topic: &str, payload: &[u8]) {
        if topic != topics::PLUGIN_STATE {
            return;
        }
        let Some(event) = PluginStateEvent::parse(payload) else {
            return;
        };
        if event.state == PluginState::RUNNING {
            cards::send_all();
        }
        // A stopped or faulted plugin must not be called again: take down everything it
        // put up. Disabled is only paused, so its panels stay registered but are skipped.
        if event.state == PluginState::STOPPED || event.state == PluginState::FAULTED {
            registry::remove_owner(event.plugin.raw());
            notices::take_all(event.plugin.raw());
            cards::forget(event.plugin.raw());
            shared::refresh();
        }
    }
}

/// What the hotkeys and the `toggle` command share.
fn toggle(what: &str) {
    match what {
        "console" => shared::toggle_console(),
        "settings" => shared::toggle_editor(),
        "mouse" => shared::toggle_grab(),
        _ => {}
    }
}

export_plugin!(OverlayPlugin);
