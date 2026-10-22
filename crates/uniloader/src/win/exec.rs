//! Running console scripts on Windows: the platform half of [`crate::run`] (what an outcome
//! asks of plugins), the loader's one [`Buffer`], and key events turned into scripts and
//! hotkey callbacks.

use uniloader_api::Status;
use uniloader_core::binds::{Edge, Event, Fire};

use crate::buffer::Buffer;
use crate::console::{Ctx, Outcome, Source};
use crate::run::{Effects, Job};
use crate::state::State;

use super::{console, dispatch, lifecycle, speed, state};

/// Every script goes through this one buffer.
pub(super) static BUFFER: Buffer = Buffer::new();

/// The live loader: the global state and real plugin calls.
pub(super) struct Platform;

impl Effects for Platform {
    fn with_state<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        f(&mut state())
    }

    fn perform(&self, outcome: Outcome) -> (Status, Vec<String>) {
        for persist in outcome.persist {
            persist.write();
        }
        dispatch::deliver(outcome.notify);
        if let Some(action) = outcome.hotkey {
            dispatch::hotkey(&action, Edge::Press);
        }
        if let Some((op, name)) = outcome.lifecycle {
            // Performed out here, with no lock held: it loads a DLL and calls into it.
            let (status, lines) = lifecycle::perform(op, &name);
            let mut guard = state();
            for line in &lines {
                guard.console_print(line.clone());
            }
            return (status, lines);
        }
        match outcome.command {
            // The loader's own commands: no DLL to call.
            Some((plugin, name, _)) if Some(plugin) == speed::owner() => {
                let (status, line) = match speed::action(&name) {
                    Ok(line) => (Status::OK, line),
                    Err(status) => (status, format!("{name} failed: {status:?}")),
                };
                state().console_print(line.clone());
                (status, vec![line])
            }
            // A plugin command prints through `console_print`, which lands in the buffer.
            Some((plugin, name, args)) => {
                let before = state().console_mark();
                let status = dispatch::command(plugin, &name, &args);
                let after = state().console_since(before);
                (status, after)
            }
            None => (outcome.status, Vec::new()),
        }
    }

    fn printed(&self, lines: &[String]) {
        for line in lines {
            console::print(line);
        }
    }
}

/// Run a script now (or as soon as the console is free) and return its status and output.
pub(crate) fn run_script(ctx: Ctx, text: &str) -> (Status, Vec<String>) {
    match Job::new(ctx, text) {
        Ok(job) => BUFFER.submit(&Platform, job),
        Err(e) => {
            let line = e.to_string();
            state().console_print(line.clone());
            (Status::INVALID_ARGUMENT, vec![line])
        }
    }
}

/// Run what key events ask for: hotkey callbacks directly, scripts through the buffer.
pub(super) fn deliver_keys(events: Vec<Event>) {
    for event in events {
        log::debug!("{} {:?} {:?}", event.chord, event.edge, event.fire);
        match event.fire {
            Fire::Hotkey(action) => dispatch::hotkey(&action, event.edge),
            Fire::Script(text) => match Job::new(Ctx::from(Source::Key), &text) {
                Ok(job) => BUFFER.post(job),
                Err(e) => log::warn!("bind {}: {e}", event.chord),
            },
        }
    }
}

/// Run `config/autoexec.cfg` once, if the user wrote one.
pub(super) fn autoexec() {
    let file = state().paths.cfg_file("autoexec");
    if file.exists() {
        log::info!("running {}", file.display());
        let (status, _) = run_script(Ctx::from(Source::Startup), "exec autoexec");
        if status != Status::OK {
            log::warn!("{} ended with {status:?}", file.display());
        }
    }
}
