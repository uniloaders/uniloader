//! The self-test: every host function, each checked, one log line per check.

use std::sync::atomic::Ordering;

use uniloader_sdk::{
    ConsoleKind, Host, KeyModifiers, LogLevel, ModuleKind, PluginState, SettingKind,
};

use crate::{KitchenSink, MathService};

/// Collects check results.
struct Checks {
    passed: u32,
    failed: u32,
}

impl Checks {
    fn check(&mut self, what: &str, ok: bool) {
        if ok {
            self.passed += 1;
            log::info!("ok   {what}");
        } else {
            self.failed += 1;
            log::error!("FAIL {what}");
        }
    }
}

fn settings(c: &mut Checks, sink: &KitchenSink, host: &Host) {
    c.check("own setting reads", host.get("level").as_deref() == Ok("3"));
    c.check("typed read", host.get_as::<f64>("scale") == Ok(1.5));
    let before = sink.setting_changes.load(Ordering::Relaxed);
    c.check("own setting writes", host.set("level", "7").is_ok());
    c.check(
        "change callback fired",
        sink.setting_changes.load(Ordering::Relaxed) == before + 1,
    );
    c.check("out of range refused", host.set("level", "11").is_err());
    c.check("bad choice refused", host.set("mode", "turbo").is_err());
    c.check("restored", host.set("level", "3").is_ok());
    c.check(
        "transient setting writes",
        host.set("counter", "42").is_ok() && host.get("counter").as_deref() == Ok("42"),
    );
    if host.find_plugin("hello").is_some() {
        c.check(
            "another plugin's setting reads",
            host.get("hello.greeting").is_ok(),
        );
    }
}

fn registries(c: &mut Checks, host: &Host) {
    let modules = host.modules();
    let me = modules.iter().find(|m| m.name == "kitchen");
    c.check(
        "modules list me as a running plugin",
        me.is_some_and(|m| m.kind == ModuleKind::PLUGIN && m.state == PluginState::RUNNING),
    );
    c.check(
        "modules carry title and version",
        me.is_some_and(|m| m.title == "Kitchen sink" && m.version == env!("CARGO_PKG_VERSION")),
    );
    let settings = host.settings(Some("kitchen"));
    c.check("six settings enumerated", settings.len() == 6);
    c.check(
        "enum setting carries its choices",
        settings
            .iter()
            .any(|s| s.key == "mode" && s.kind == SettingKind::Enum && s.choices.len() == 2),
    );
    c.check(
        "advanced flag survives",
        settings.iter().any(|s| s.key == "motto" && s.advanced),
    );
    let hotkeys = host.hotkeys();
    c.check(
        "hotkey enumerated with its binding",
        hotkeys
            .iter()
            .any(|h| h.name == "kitchen.poke" && h.binding == "ctrl+f8"),
    );
    c.check("rebind", host.rebind("kitchen.poke", "alt+f8, f7").is_ok());
    c.check(
        "rebind visible",
        host.hotkeys()
            .iter()
            .any(|h| h.name == "kitchen.poke" && h.binding == "alt+f8, f7"),
    );
    c.check(
        "rebind back",
        host.rebind("kitchen.poke", "ctrl+f8").is_ok(),
    );
    c.check(
        "bad binding refused",
        host.rebind("kitchen.poke", "nokey").is_err(),
    );
}

// Looking a service up is unsafe: the table's type is the caller's claim.
#[allow(unsafe_code)]
fn services(c: &mut Checks, host: &Host) {
    // SAFETY: `kitchen.math` v1 is `MathService`, which this plugin registered itself.
    let math = unsafe { host.service::<MathService>("kitchen.math") };
    c.check(
        "service found with version and provider",
        math.is_some_and(|m| m.version == 1 && m.provider == host.me()),
    );
    c.check(
        "service callable",
        math.is_some_and(|m| (m.table.add)(2, 3) == 5),
    );
    // SAFETY: the type is irrelevant for a lookup that must fail.
    let missing = unsafe { host.service::<MathService>("nobody.provides.this") };
    c.check("missing service is None", missing.is_none());
    let mut ran = false;
    c.check(
        "guarded call runs",
        host.guarded(host.me(), || ran = true).is_ok() && ran,
    );
    c.check(
        "unknown plugin not found",
        host.find_plugin("ghost-plugin-that-is-not-there").is_none(),
    );
}

fn messaging(c: &mut Checks, sink: &KitchenSink, host: &Host) {
    let reply = host.message(host.me(), "echo", b"ping");
    c.check(
        "message with reply",
        reply
            .as_ref()
            .is_ok_and(|r| r.as_deref() == Some(&b"ping"[..])),
    );
    c.check(
        "unknown message topic fails",
        host.message(host.me(), "nope", b"").is_err(),
    );
    if let Some(hello) = host.find_plugin("hello") {
        c.check(
            "found another plugin and its state",
            host.plugin_state(hello) == Some(PluginState::RUNNING),
        );
        host.publish("hello.ping", b"kitchen");
    }
    let before = sink.events.load(Ordering::Relaxed);
    host.publish("kitchen.ping", b"");
    c.check(
        "own broadcast not echoed back",
        sink.events.load(Ordering::Relaxed) == before,
    );
}

fn console(c: &mut Checks, sink: &KitchenSink, host: &Host) {
    let (status, lines) = host.console_capture("get kitchen.level");
    c.check(
        "console capture",
        status.is_ok() && lines == ["kitchen.level = 3"],
    );
    let before = sink.commands.load(Ordering::Relaxed);
    c.check("console exec", host.console_exec("kitchen.echo hi").is_ok());
    c.check(
        "own command dispatched",
        sink.commands.load(Ordering::Relaxed) == before + 1,
    );
    c.check(
        "unknown command fails",
        host.console_exec("kitchen.missing").is_err(),
    );
    let (_, plugins) = host.console_capture("plugins");
    c.check(
        "plugins listing shows the service",
        plugins
            .iter()
            .any(|l| l == "service kitchen.math v1 from kitchen"),
    );
    let (_, mark) = host.console_read(0);
    host.log(LogLevel::INFO, "console read marker");
    host.console_print("console read output");
    let (fresh, next) = host.console_read(mark);
    c.check(
        "console read sees log and output",
        next > mark
            && fresh.iter().any(|l| {
                l.kind == ConsoleKind::LOG
                    && l.level == LogLevel::INFO
                    && l.text.ends_with("console read marker")
            })
            && fresh.iter().any(|l| {
                l.kind == ConsoleKind::OUTPUT && l.text.ends_with("[kitchen] console read output")
            }),
    );
    // Claimed first, so the press reaches this plugin whoever else holds the keyboard (the
    // overlay does while a window is open); giving the claim up hands it back to them.
    host.capture_input(true);
    let before = sink.hotkeys.load(Ordering::Relaxed);
    host.hotkey_press(0x58, KeyModifiers::default());
    c.check(
        "scan code press fires its binding while capturing",
        sink.hotkeys.load(Ordering::Relaxed) == before + 1,
    );
    host.hotkey_press(0x58, KeyModifiers(KeyModifiers::CTRL));
    c.check(
        "modifiers must match",
        sink.hotkeys.load(Ordering::Relaxed) == before + 1,
    );
    host.capture_input(false);
    let names = host.console_names();
    c.check(
        "console names",
        [
            "plugin load",
            "kitchen.echo",
            "kitchen.level",
            "kitchen.poke",
        ]
        .iter()
        .all(|n| names.iter().any(|have| have == n)),
    );
}

fn environment(c: &mut Checks, host: &Host) {
    c.check("loader version", !host.loader_version().is_empty());
    c.check("game directory", host.game_dir().is_dir());
    c.check(
        "plugin directory",
        host.plugin_dir().join("kitchen_sink.dll").is_file(),
    );
    c.check("executable", host.exe_path().is_file());
    c.check("module base", !host.module_base().is_null());
    c.check("environment", !host.env().is_empty());
}

/// Run every check and log the total.
pub(crate) fn run(sink: &KitchenSink, host: &Host) {
    let mut c = Checks {
        passed: 0,
        failed: 0,
    };
    settings(&mut c, sink, host);
    registries(&mut c, host);
    services(&mut c, host);
    messaging(&mut c, sink, host);
    console(&mut c, sink, host);
    environment(&mut c, host);
    crate::hooks::run(&mut |what, ok| c.check(what, ok), host);
    if let Some(overlay) = &sink.overlay {
        crate::overlay::checks(&mut |what, ok| c.check(what, ok), overlay, host);
    }
    log::info!(
        "kitchen self-test: {} passed, {} failed",
        c.passed,
        c.failed
    );
}
