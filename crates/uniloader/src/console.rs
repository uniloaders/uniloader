//! Executes parsed console lines against the [`State`]. Returns the plugin calls the
//! Windows side must make after unlocking, so this stays testable without a game.
//!
//! From the DayZ loader's `console.rs` (`eaebd9b`) without the DayZ commands.

use uniloader_api::{PluginHandle, Status};
use uniloader_core::console::{self, Line, PluginOp, BUILTIN_HELP};
use uniloader_core::names;

use crate::state::{Notify, Persist, State};

#[path = "console_binds.rs"]
mod binds;

/// Who submitted a script. Decides what it may do: RCON may not change the `rcon_*`
/// settings, and key binds stop while a plugin captures the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The console window or an overlay console, typed by the user.
    Console,
    /// A plugin's `console_exec` or `console_capture`.
    Plugin,
    /// A key bind.
    Key,
    /// A cfg file the loader runs by itself (`autoexec.cfg`).
    Startup,
}

/// The context a command runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ctx {
    /// The plugin running it, which lets a plugin name its own settings without its prefix.
    pub caller: Option<PluginHandle>,
    /// Who submitted it.
    pub source: Source,
}

impl Ctx {
    /// A context for `source` with no calling plugin.
    #[must_use]
    pub fn from(source: Source) -> Self {
        Ctx {
            caller: None,
            source,
        }
    }
}

/// What executing a command produced.
#[derive(Debug, PartialEq, Eq)]
pub struct Outcome {
    /// Status to return to a plugin that called `console_exec`.
    pub status: Status,
    /// Setting notifications to deliver.
    pub notify: Vec<Notify>,
    /// Files to write once the lock is released.
    pub persist: Vec<Persist>,
    /// Plugin command to run: `(plugin, name, args)`.
    pub command: Option<(PluginHandle, String, String)>,
    /// Lifecycle operation for the platform layer to perform once the lock is released.
    ///
    /// Loading a DLL and calling into it must not happen under the state lock, so the same
    /// rule as for [`Outcome::command`] applies: this module only decides what to do.
    pub lifecycle: Option<(PluginOp, String)>,
    /// Hotkey action (`<plugin>.<action>`) to fire.
    pub hotkey: Option<String>,
    /// Commands to run next, before the rest of the script: an alias's body.
    pub expand: Vec<String>,
    /// `exec <name> <args>`: the runner reads the cfg file, outside the lock.
    pub exec: Option<(String, String)>,
    /// `wait`: run the rest of the script this many milliseconds later.
    pub wait: Option<u64>,
}

impl Default for Outcome {
    fn default() -> Self {
        Outcome {
            status: Status::OK,
            notify: Vec::new(),
            persist: Vec::new(),
            command: None,
            lifecycle: None,
            hotkey: None,
            expand: Vec::new(),
            exec: None,
            wait: None,
        }
    }
}

pub(crate) fn fail(state: &mut State, status: Status, message: String) -> Outcome {
    state.console_print(message);
    Outcome {
        status,
        ..Outcome::default()
    }
}

pub(crate) fn print_all(state: &mut State, lines: Vec<String>, when_empty: &str) -> Outcome {
    if lines.is_empty() && !when_empty.is_empty() {
        state.console_print(when_empty.to_owned());
    }
    for line in lines {
        state.console_print(line);
    }
    Outcome::default()
}

/// The length of a tick, what `wait` without an argument waits.
pub const TICK_MS: u64 = 16;

/// Execute one command (one piece of a script, as `script::commands` split it). Output goes
/// to the state's console buffer.
pub fn execute(state: &mut State, ctx: &Ctx, text: &str) -> Outcome {
    if !text.trim().is_empty() {
        state.console_print(format!("> {}", text.trim()));
    }
    let parsed = match console::parse(text) {
        Ok(p) => p,
        Err(e) => return fail(state, Status::INVALID_ARGUMENT, e.to_string()),
    };
    let caller = ctx.caller;
    match parsed {
        Line::Empty => Outcome::default(),
        Line::Help(None) => {
            for (usage, help) in BUILTIN_HELP {
                state.console_print(format!("{usage:<28} {help}"));
            }
            Outcome::default()
        }
        Line::Help(Some(name)) => help_for(state, &name),
        Line::List(prefix) => list(state, prefix.as_deref()),
        Line::Plugin(op, name) => plugin_op(state, op, name.as_deref()),
        Line::Hooks => {
            let lines = (state.hook_lines)();
            print_all(state, lines, "no hooks installed")
        }
        Line::Quit => {
            let lines = (state.quit)();
            print_all(state, lines, "closing the game")
        }
        Line::Get(name) => get(state, caller, &name),
        Line::Set(name, value) => set(state, ctx, &name, &value),
        Line::Variable(name, value) => binds::undotted(state, ctx, &name, value.as_deref()),
        Line::Command(name, args) => command(state, ctx, &name, &args),
        Line::Echo(text) => print_all(state, vec![text], ""),
        Line::Wait(ms) => Outcome {
            wait: Some(ms.unwrap_or(TICK_MS)),
            ..Outcome::default()
        },
        Line::Exec(name, args) => Outcome {
            exec: Some((name, args)),
            ..Outcome::default()
        },
        other => binds::execute(state, ctx, other),
    }
}

/// `plugins`: one line per plugin, then the ones waiting for a dependency.
fn list_plugins(state: &mut State) -> Outcome {
    let mut lines: Vec<String> = state
        .plugins
        .iter()
        .map(|p| {
            format!(
                "{} {} ({}) {}",
                p.name(),
                p.info.version,
                p.file,
                p.state().as_str()
            )
        })
        .collect();
    lines.extend(
        state
            .pending
            .iter()
            .map(|(name, reason)| format!("{name} waiting: {reason}")),
    );
    // Not plugins, but loaded by the loader and part of what is running in the game.
    lines.extend(state.foreign.iter().map(|f| {
        let version = if f.version.is_empty() {
            String::new()
        } else {
            format!(" {}", f.version)
        };
        format!("  {}{version} ({}) {}", f.name, f.file, f.kind)
    }));
    let services: Vec<String> = state
        .services
        .iter()
        .map(|(name, s)| {
            let provider = state.plugin(s.provider).map_or("?", |p| p.name());
            format!("service {name} v{} from {provider}", s.version)
        })
        .collect();
    lines.extend(services);
    print_all(state, lines, "no plugins loaded")
}

/// The plugin lifecycle commands.
///
/// Listing is answered here; everything that calls into a DLL is handed back to the platform
/// layer, which performs it after the state lock is gone.
fn plugin_op(state: &mut State, op: PluginOp, name: Option<&str>) -> Outcome {
    match op {
        PluginOp::List => list_plugins(state),
        PluginOp::Deps => {
            let lines: Vec<String> = state
                .plugins
                .iter()
                .filter(|p| name.is_none_or(|n| p.name() == n))
                .flat_map(|p| {
                    if p.dependencies.is_empty() {
                        vec![format!("{} needs nothing", p.name())]
                    } else {
                        p.dependencies
                            .iter()
                            .map(|d| format!("{} needs {d}", p.name()))
                            .collect()
                    }
                })
                .collect();
            if lines.is_empty() {
                return fail(
                    state,
                    Status::NOT_FOUND,
                    format!("no plugin named {}", name.unwrap_or_default()),
                );
            }
            print_all(state, lines, "")
        }
        PluginOp::Reload => fail(
            state,
            Status::UNSUPPORTED,
            "reloading is not possible in-process: the DLL's hooks, threads and the pointers \
             plugins hold would all have to come back identical. Rebuild and restart the game; \
             `plugin stop` plus `plugin load` only works for a DLL under a new file name."
                .to_owned(),
        ),
        // The name is guaranteed by the parser for these.
        PluginOp::Load | PluginOp::Stop | PluginOp::Enable | PluginOp::Disable => {
            let Some(name) = name else {
                return fail(
                    state,
                    Status::INVALID_ARGUMENT,
                    format!("usage: plugin {} <name>", op.as_str()),
                );
            };
            Outcome {
                lifecycle: Some((op, name.to_owned())),
                ..Outcome::default()
            }
        }
    }
}

pub(crate) fn get(state: &mut State, caller: Option<PluginHandle>, name: &str) -> Outcome {
    match state.get_setting(caller, name) {
        Ok(value) => {
            state.console_print(format!("{name} = {value}"));
            Outcome::default()
        }
        Err(status) => fail(state, status, format!("unknown setting {name}")),
    }
}

pub(crate) fn set(state: &mut State, ctx: &Ctx, name: &str, value: &str) -> Outcome {
    let caller = ctx.caller;
    match state.set_setting(caller, name, value) {
        Ok(changed) => {
            if let Ok(v) = state.get_setting(caller, name) {
                state.console_print(format!("{name} = {v}"));
            }
            Outcome {
                notify: changed.notify.into_iter().collect(),
                persist: changed.persist.into_iter().collect(),
                ..Outcome::default()
            }
        }
        Err((status, message)) => fail(state, status, message),
    }
}

fn command(state: &mut State, ctx: &Ctx, name: &str, args: &str) -> Outcome {
    let caller = ctx.caller;
    // A dotted name is a setting when one matches, then a plugin command, then a hotkey
    // action.
    if state.get_setting(caller, name).is_ok() {
        return if args.is_empty() {
            get(state, caller, name)
        } else {
            set(state, ctx, name, &uniloader_core::script::value(args))
        };
    }
    let is_command = names::split_qualified(name)
        .and_then(|(plugin, cmd)| Some((state.find_plugin(plugin)?, cmd)))
        .is_some_and(|(handle, cmd)| {
            state
                .plugin(handle)
                .is_some_and(|p| p.commands.contains_key(cmd))
        });
    if !is_command && args.is_empty() {
        if let Some(outcome) = binds::action(state, name) {
            return outcome;
        }
    }
    let Some((plugin, cmd)) = names::split_qualified(name) else {
        return fail(state, Status::NOT_FOUND, format!("unknown command {name}"));
    };
    let Some(handle) = state.find_plugin(plugin) else {
        return fail(
            state,
            Status::NOT_FOUND,
            format!("no plugin named {plugin}"),
        );
    };
    let known = state
        .plugin(handle)
        .is_some_and(|p| p.enabled && p.commands.contains_key(cmd));
    if !known {
        return fail(state, Status::NOT_FOUND, format!("unknown command {name}"));
    }
    Outcome {
        command: Some((handle, cmd.to_owned(), args.to_owned())),
        ..Outcome::default()
    }
}

fn help_for(state: &mut State, name: &str) -> Outcome {
    let mut lines = Vec::new();
    if let Some((plugin, key)) = names::split_qualified(name) {
        if let Some(record) = state.find_plugin(plugin).and_then(|h| state.plugin(h)) {
            if let Some(desc) = record.settings.desc(key) {
                lines.push(format!(
                    "{name}: {} ({:?}, default {})",
                    desc.title, desc.kind, desc.default
                ));
                if !desc.description.is_empty() {
                    lines.push(format!("  {}", desc.description));
                }
                if !desc.choices.is_empty() {
                    lines.push(format!("  choices: {}", desc.choices.join(", ")));
                }
            }
            if let Some(cmd) = record.commands.get(key) {
                lines.push(format!("{name} {}: {}", cmd.usage, cmd.help));
            }
        }
    }
    if lines.is_empty() {
        return fail(
            state,
            Status::NOT_FOUND,
            format!("nothing known about {name}"),
        );
    }
    print_all(state, lines, "")
}

fn list(state: &mut State, prefix: Option<&str>) -> Outcome {
    let mut lines = Vec::new();
    for record in &state.plugins {
        for (desc, value) in record.settings.iter() {
            lines.push(format!("{}.{} = {value}", record.name(), desc.key));
        }
        for (cmd, info) in &record.commands {
            lines.push(format!("{}.{cmd} {}", record.name(), info.usage));
        }
    }
    lines.retain(|l| prefix.is_none_or(|p| l.starts_with(p)));
    print_all(state, lines, "nothing registered")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::{desc, started, state};
    use crate::state::{CommandInfo, Phase};

    const CTX: Ctx = Ctx {
        caller: None,
        source: Source::Console,
    };

    fn fixture() -> (State, PluginHandle) {
        let mut s = state();
        let h = started(&mut s, "vr");
        s.register_setting(h, desc("ipd"))
            .unwrap_or_else(|e| panic!("{e:?}"));
        s.register_command(
            h,
            "recenter",
            CommandInfo {
                help: "Recenter.".into(),
                usage: String::new(),
            },
        )
        .unwrap_or_else(|e| panic!("{e:?}"));
        s.register_hotkey(h, "toggle", "Toggle", "f12")
            .unwrap_or_else(|e| panic!("{e:?}"));
        s.phase = Phase::Running;
        s.set_enabled(h, true);
        (s, h)
    }

    fn last(s: &State) -> &str {
        s.console.back().map_or("", |line| line.text.as_str())
    }

    #[test]
    fn get_set_and_shorthand() {
        let (mut s, h) = fixture();
        assert_eq!(execute(&mut s, &CTX, "get vr.ipd").status, Status::OK);
        assert_eq!(last(&s), "vr.ipd = 1");
        let out = execute(&mut s, &CTX, "set vr.ipd 3");
        assert_eq!(
            out.notify,
            vec![Notify::SettingChanged {
                plugin: h,
                key: "ipd".into(),
                value: "3".into()
            }]
        );
        assert_eq!(last(&s), "vr.ipd = 3");
        assert_eq!(execute(&mut s, &CTX, "vr.ipd 5").notify.len(), 1);
        assert_eq!(execute(&mut s, &CTX, "vr.ipd").status, Status::OK);
        assert_eq!(last(&s), "vr.ipd = 5");
        assert_eq!(
            execute(&mut s, &CTX, "vr.ipd 50").status,
            Status::INVALID_ARGUMENT
        );
        assert_eq!(
            execute(&mut s, &CTX, "set vr.ipd").status,
            Status::INVALID_ARGUMENT
        );
        let _ = std::fs::remove_dir_all(&s.paths.game_dir);
    }

    #[test]
    fn commands_route_to_plugin() {
        let (mut s, h) = fixture();
        let out = execute(&mut s, &CTX, "vr.recenter now");
        assert_eq!(out.command, Some((h, "recenter".into(), "now".into())));
        assert_eq!(
            execute(&mut s, &CTX, "vr.missing").status,
            Status::NOT_FOUND
        );
        assert_eq!(execute(&mut s, &CTX, "nope.x").status, Status::NOT_FOUND);
        assert_eq!(last(&s), "no plugin named nope");
        s.set_enabled(h, false);
        assert_eq!(
            execute(&mut s, &CTX, "vr.recenter").status,
            Status::NOT_FOUND
        );
    }

    #[test]
    fn plugin_lifecycle_lines_are_handed_to_the_platform_layer() {
        let (mut s, h) = fixture();
        s.set_dependencies(
            h,
            [
                "library openxr_loader.dll".to_owned(),
                "plugin a (optional)".to_owned(),
            ],
        );
        execute(&mut s, &CTX, "plugins");
        assert_eq!(last(&s), "vr 1 (vr.dll) running");
        execute(&mut s, &CTX, "plugin deps");
        assert_eq!(last(&s), "vr needs plugin a (optional)");
        assert_eq!(
            execute(&mut s, &CTX, "plugin deps nope").status,
            Status::NOT_FOUND
        );

        let out = execute(&mut s, &CTX, "plugin load hello");
        assert_eq!(out.lifecycle, Some((PluginOp::Load, "hello".to_owned())));
        assert_eq!(
            out.status,
            Status::OK,
            "the platform layer reports the real one"
        );
        let out = execute(&mut s, &CTX, "plugin disable vr");
        assert_eq!(out.lifecycle, Some((PluginOp::Disable, "vr".to_owned())));

        // Reload is refused in one place, with the reason, rather than silently doing less.
        let out = execute(&mut s, &CTX, "plugin reload vr");
        assert_eq!(out.status, Status::UNSUPPORTED);
        assert!(last(&s).contains("not possible in-process"));
        assert_eq!(
            execute(&mut s, &CTX, "plugin stop").status,
            Status::INVALID_ARGUMENT
        );
        let _ = std::fs::remove_dir_all(&s.paths.game_dir);
    }

    #[test]
    fn foreign_modules_are_listed_after_the_plugins() {
        let (mut s, _) = fixture();
        s.foreign.push(crate::state::Foreign {
            name: "UE4SS".into(),
            file: "ue4ss/UE4SS.dll".into(),
            version: "3.0.1".into(),
            kind: "library",
        });
        execute(&mut s, &CTX, "plugins");
        assert_eq!(last(&s), "  UE4SS 3.0.1 (ue4ss/UE4SS.dll) library");
        let vr = s.find_plugin("vr").unwrap_or(PluginHandle::NONE);
        s.register_service(vr, "overlay", 2, 0x10)
            .unwrap_or_else(|e| panic!("{e:?}"));
        execute(&mut s, &CTX, "plugins");
        assert_eq!(last(&s), "service overlay v2 from vr");
        let _ = std::fs::remove_dir_all(&s.paths.game_dir);
    }

    #[test]
    fn hooks_and_quit_print_what_the_platform_layer_supplies() {
        let (mut s, _) = fixture();
        execute(&mut s, &CTX, "hooks");
        assert_eq!(last(&s), "no hooks installed");
        s.hook_lines = || vec!["vr detour 0x1000 camera".to_owned()];
        execute(&mut s, &CTX, "hooks");
        assert_eq!(last(&s), "vr detour 0x1000 camera");
        execute(&mut s, &CTX, "quit");
        assert_eq!(last(&s), "closing the game needs the platform layer");
    }

    #[test]
    fn help_list_hotkeys_and_plugins() {
        let (mut s, _) = fixture();
        execute(&mut s, &CTX, "help");
        assert!(s.console.iter().any(|l| l.text.starts_with("plugins")));
        execute(&mut s, &CTX, "help vr.recenter");
        assert_eq!(last(&s), "vr.recenter : Recenter.");
        assert_eq!(
            execute(&mut s, &CTX, "help vr.what").status,
            Status::NOT_FOUND
        );
        execute(&mut s, &CTX, "list vr.i");
        assert_eq!(last(&s), "vr.ipd = 1");
        execute(&mut s, &CTX, "list zzz");
        assert_eq!(last(&s), "nothing registered");
        execute(&mut s, &CTX, "hotkeys");
        assert_eq!(
            last(&s),
            "f12              \"action vr.toggle\"  (default vr.toggle)"
        );
        execute(&mut s, &CTX, "plugins");
        assert_eq!(last(&s), "vr 1 (vr.dll) running");
        assert_eq!(execute(&mut s, &CTX, "   "), Outcome::default());
    }
}
