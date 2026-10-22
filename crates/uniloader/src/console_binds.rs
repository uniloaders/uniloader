//! The bind, alias and setting-stepping built-ins of the console: `bind`, `unbind`,
//! `unbindall`, `binds`, `binds reset`, `rebind`, `alias`, `aliases`, `action`, `toggle`,
//! `incrementvar` and `reset`. Every change to the bind table returns `binds.cfg` to write
//! once the lock is released.

use uniloader_api::Status;
use uniloader_core::binds::{self, Origin, Target};
use uniloader_core::console::{Line, BUILTIN_NAMES};
use uniloader_core::{keys, script, settings};

use super::{fail, get, print_all, set, Ctx, Outcome};
use crate::state::State;

/// Run a bind-family line. Lines the caller handles itself never reach here.
pub(super) fn execute(state: &mut State, ctx: &Ctx, line: Line) -> Outcome {
    match line {
        Line::Bind(chord, None) => show_bind(state, &chord),
        Line::Bind(chord, Some(script)) => bind(state, ctx, &chord, &script),
        Line::Unbind(chord) => unbind(state, &chord),
        Line::UnbindAll => {
            state.binds.unbind_all();
            state.console_print(
                "every key unbound; `binds reset all` brings the plugins' suggestions back"
                    .to_owned(),
            );
            saved(state)
        }
        Line::Binds(filter) => list(state, filter.as_deref()),
        Line::BindsReset(what) => reset_binds(state, &what),
        Line::Rebind(name, chords) => rebind(state, &name, &chords),
        Line::Alias(name, None) => match state.binds.alias_script(&name) {
            Some(body) => {
                let line = format!("alias {name} {}", script::quote(body));
                print_all(state, vec![line], "")
            }
            None => fail(state, Status::NOT_FOUND, format!("no alias named {name}")),
        },
        Line::Alias(name, Some(body)) => alias(state, ctx, &name, &body),
        Line::Aliases => {
            let lines = state
                .binds
                .aliases()
                .map(|(name, body)| format!("alias {name} {}", script::quote(body)))
                .collect();
            print_all(state, lines, "no aliases")
        }
        Line::Action(name) => action(state, &name).unwrap_or_else(|| {
            fail(
                state,
                Status::NOT_FOUND,
                format!("no hotkey action named {name}"),
            )
        }),
        Line::Toggle(name, values) => toggle(state, ctx, &name, &values),
        Line::IncrementVar(name, [min, max, step]) => {
            increment(state, ctx, &name, (min, max, step))
        }
        Line::Reset(name) => match state.setting_desc(ctx.caller, &name) {
            Some(desc) => {
                let default = desc.default.clone();
                set(state, ctx, &name, &default)
            }
            None => fail(state, Status::NOT_FOUND, format!("unknown setting {name}")),
        },
        other => fail(
            state,
            Status::ERROR,
            format!("{other:?} reached the bind commands"),
        ),
    }
}

/// `binds.cfg` with the current table, as the outcome of a change.
fn saved(state: &mut State) -> Outcome {
    Outcome {
        persist: vec![state.save_binds()],
        ..Outcome::default()
    }
}

/// One chord, or `None` after printing why not.
fn parse_chord(state: &mut State, text: &str) -> Option<keys::Chord> {
    binds::chord(text)
        .map_err(|e| state.console_print(format!("{text}: {e}")))
        .ok()
}

fn invalid() -> Outcome {
    Outcome {
        status: Status::INVALID_ARGUMENT,
        ..Outcome::default()
    }
}

fn show_bind(state: &mut State, text: &str) -> Outcome {
    let Some(chord) = parse_chord(state, text) else {
        return invalid();
    };
    let lines: Vec<String> = state
        .binds
        .rows()
        .into_iter()
        .filter(|row| row.chord == chord && !matches!(row.origin, Origin::Removed(_)))
        .map(|row| {
            format!(
                "{} = {}  ({})",
                row.chord,
                script::quote(&row.script),
                row.origin
            )
        })
        .collect();
    print_all(state, lines, &format!("{chord} is not bound"))
}

fn bind(state: &mut State, ctx: &Ctx, text: &str, body: &str) -> Outcome {
    let Some(chord) = parse_chord(state, text) else {
        return invalid();
    };
    if let Err(e) = script::commands(body) {
        return fail(state, Status::INVALID_ARGUMENT, e.to_string());
    }
    if let Some(refused) = refused_body(state, ctx, body) {
        return refused;
    }
    state.binds.bind(chord, body);
    state.console_print(format!("{chord} = {}", script::quote(body)));
    let plain = chord.modifiers == keys::Modifiers::default() && chord.scancode.is_none();
    if plain && matches!(chord.vk, 0x01 | 0x02) {
        state.console_print(format!(
            "note: {chord} also clicks in the game; the bind cannot swallow it"
        ));
    }
    saved(state)
}

fn unbind(state: &mut State, text: &str) -> Outcome {
    let Some(chord) = parse_chord(state, text) else {
        return invalid();
    };
    if !state.binds.unbind(chord) {
        return print_all(state, Vec::new(), &format!("{chord} was not bound"));
    }
    state.console_print(format!("{chord} unbound"));
    saved(state)
}

fn list(state: &mut State, filter: Option<&str>) -> Outcome {
    let lines: Vec<String> = state
        .binds
        .rows()
        .into_iter()
        .map(|row| {
            format!(
                "{:<16} {}  ({})",
                row.chord.to_string(),
                script::quote(&row.script),
                row.origin
            )
        })
        .filter(|line| filter.is_none_or(|f| line.contains(f)))
        .collect();
    print_all(state, lines, "nothing bound")
}

fn reset_binds(state: &mut State, what: &str) -> Outcome {
    if what == "all" {
        state.binds.reset_all();
        state.console_print("every plugin bind is back on its suggested keys".to_owned());
        return saved(state);
    }
    if what.contains('.') && !what.contains('+') {
        return match state.binds.reset_entry(what) {
            Ok(()) => {
                state.console_print(format!("{what} is back on its suggested keys"));
                saved(state)
            }
            Err(e) => fail(state, Status::NOT_FOUND, e.to_string()),
        };
    }
    let Some(chord) = parse_chord(state, what) else {
        return invalid();
    };
    state.binds.reset_chord(chord);
    state.console_print(format!("{chord} is back to the plugins' suggestions"));
    saved(state)
}

fn rebind(state: &mut State, name: &str, text: &str) -> Outcome {
    let chords = match keys::parse_list(text) {
        Ok(chords) => chords,
        Err(e) => return fail(state, Status::INVALID_ARGUMENT, format!("{text}: {e}")),
    };
    let binding = keys::format_list(&chords);
    if state.binds.rebind(name, chords) {
        state.console_print(format!("{name} = {binding}"));
    } else {
        state.console_print(format!(
            "{name} = {binding} (no plugin registered it yet; kept for when one does)"
        ));
    }
    saved(state)
}

fn alias(state: &mut State, ctx: &Ctx, name: &str, body: &str) -> Outcome {
    let builtin = BUILTIN_NAMES
        .iter()
        .any(|b| b.split(' ').next() == Some(name));
    if builtin || matches!(name, "hotkeys" | "keys" | "ls" | "exit" | "?") {
        return fail(
            state,
            Status::INVALID_ARGUMENT,
            format!("{name} is a built-in command"),
        );
    }
    if let Err(e) = script::commands(body) {
        return fail(state, Status::INVALID_ARGUMENT, e.to_string());
    }
    if let Some(refused) = refused_body(state, ctx, body) {
        return refused;
    }
    if let Err(e) = state.binds.alias(name, body) {
        return fail(state, Status::INVALID_ARGUMENT, e.to_string());
    }
    if body.trim().is_empty() {
        state.console_print(format!("alias {name} deleted"));
    } else {
        state.console_print(format!("alias {name} {}", script::quote(body)));
    }
    saved(state)
}

/// Why a bind or alias may not hold `body`, as the failed outcome: it would carry a secret's
/// value, or (made over RCON, and so run later as the user's) touch the RCON settings.
fn refused_body(state: &mut State, ctx: &Ctx, body: &str) -> Option<Outcome> {
    let message = if crate::secret::carries_secret(state, ctx, body) {
        "a bind or alias cannot hold a secret setting's value; set it in the console"
    } else if ctx.source == super::Source::Rcon && body.to_ascii_lowercase().contains("rcon") {
        "over RCON, a bind or alias cannot mention the rcon settings"
    } else {
        return None;
    };
    Some(fail(state, Status::INVALID_ARGUMENT, message.to_owned()))
}

/// `action <plugin.action>`, also reached by a dotted name that is no setting or command.
pub(super) fn action(state: &mut State, name: &str) -> Option<Outcome> {
    let entry = state.binds.entry(name)?;
    Some(match &entry.target {
        Target::Hotkey => Outcome {
            hotkey: Some(name.to_owned()),
            ..Outcome::default()
        },
        Target::Script(body) => Outcome {
            expand: script::commands(body).unwrap_or_default(),
            ..Outcome::default()
        },
    })
}

/// A word without a dot: an alias when one has that name, otherwise a setting of the calling
/// plugin (or a bare name, which then is unknown).
pub(super) fn undotted(state: &mut State, ctx: &Ctx, name: &str, value: Option<&str>) -> Outcome {
    if let Some(body) = state.binds.alias_script(name) {
        return match script::commands(body) {
            Ok(commands) => Outcome {
                expand: commands,
                ..Outcome::default()
            },
            Err(e) => fail(
                state,
                Status::INVALID_ARGUMENT,
                format!("alias {name}: {e}"),
            ),
        };
    }
    match value {
        None => get(state, ctx.caller, name),
        Some(value) => set(state, ctx, name, value),
    }
}

fn toggle(state: &mut State, ctx: &Ctx, name: &str, values: &[String]) -> Outcome {
    let Some(desc) = state.setting_desc(ctx.caller, name) else {
        return fail(state, Status::NOT_FOUND, format!("unknown setting {name}"));
    };
    let current = state.get_setting(ctx.caller, name).unwrap_or_default();
    let cycle: Vec<String> = if values.is_empty() {
        match desc.kind {
            settings::Kind::Bool => vec!["false".to_owned(), "true".to_owned()],
            settings::Kind::Enum => desc.choices.clone(),
            _ => {
                return fail(
                    state,
                    Status::INVALID_ARGUMENT,
                    format!("toggle {name} needs the values to cycle through"),
                )
            }
        }
    } else {
        values.to_vec()
    };
    let next = cycle
        .iter()
        .position(|v| v.eq_ignore_ascii_case(&current))
        .map_or(0, |i| (i + 1) % cycle.len());
    let value = cycle.get(next).cloned().unwrap_or_default();
    set(state, ctx, name, &value)
}

fn increment(
    state: &mut State,
    ctx: &Ctx,
    name: &str,
    (min, max, step): (f64, f64, f64),
) -> Outcome {
    let Some(kind) = state.setting_desc(ctx.caller, name).map(|d| d.kind) else {
        return fail(state, Status::NOT_FOUND, format!("unknown setting {name}"));
    };
    if !matches!(kind, settings::Kind::Int | settings::Kind::Float) {
        return fail(
            state,
            Status::INVALID_ARGUMENT,
            format!("incrementvar needs a number setting; {name} is {kind:?}"),
        );
    }
    let current = state
        .get_setting(ctx.caller, name)
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(min);
    let value = step_value(current, min, max, step);
    let text = if kind == settings::Kind::Int {
        format!("{value:.0}")
    } else {
        value.to_string()
    };
    set(state, ctx, name, &text)
}

/// Source's `incrementvar`: add `step`, and past either end wrap to the other.
fn step_value(current: f64, min: f64, max: f64, step: f64) -> f64 {
    // Rounded to nine places, so ten steps of 0.1 print as 1 and not 0.9999999999999999.
    let next = ((current + step) * 1e9).round() / 1e9;
    if next > max {
        min
    } else if next < min {
        max
    } else {
        next
    }
}

#[cfg(test)]
#[path = "console_binds_tests.rs"]
mod tests;
