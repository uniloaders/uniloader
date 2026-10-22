//! Running a script: one command at a time, each executed under the state lock, with what it
//! asked of plugins performed after the lock is released, before the next command starts.
//!
//! Alias bodies and cfg files are expanded in place, ahead of the rest of the script, with the
//! nesting depth and command count limits of [`uniloader_core::script`]. `wait` ends a run
//! early and hands the rest back for the command buffer to resume.

use std::collections::VecDeque;

use uniloader_api::Status;
use uniloader_core::script::{self, ScriptError, MAX_COMMANDS, MAX_DEPTH};

use crate::config;
use crate::console::{self, Ctx, Outcome};
use crate::state::State;

/// A script on its way through the console, possibly halfway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// Who runs it.
    pub ctx: Ctx,
    /// Commands still to run, each with its nesting depth.
    queue: VecDeque<(String, usize)>,
    /// Commands run so far, expansions included.
    ran: usize,
}

impl Job {
    /// A job for a whole script.
    ///
    /// # Errors
    /// A script that does not split: broken quotes or over the size limit.
    pub fn new(ctx: Ctx, text: &str) -> Result<Self, ScriptError> {
        let queue = script::commands(text)?
            .into_iter()
            .map(|command| (command, 0))
            .collect();
        Ok(Job { ctx, queue, ran: 0 })
    }

    fn push_front(&mut self, commands: Vec<String>, depth: usize) {
        for command in commands.into_iter().rev() {
            self.queue.push_front((command, depth));
        }
    }
}

/// What a run produced.
#[derive(Debug)]
pub struct Ran {
    /// The first status that was not OK, or OK.
    pub status: Status,
    /// Every line the commands printed, without the echoed commands and log records.
    pub printed: Vec<String>,
    /// After a `wait`: how many milliseconds to wait and the rest of the job.
    pub rest: Option<(u64, Job)>,
}

/// What a run needs from the platform: the state, and the plugin calls an outcome asks for.
pub trait Effects {
    /// Run `f` with the state locked.
    fn with_state<R>(&self, f: impl FnOnce(&mut State) -> R) -> R;

    /// Perform what `outcome` asks for with no lock held: write files, deliver
    /// notifications, run the plugin command or lifecycle operation, fire the hotkey. Returns
    /// the status and the lines that printed.
    fn perform(&self, outcome: Outcome) -> (Status, Vec<String>);

    /// A run printed these lines; a console window shows them.
    fn printed(&self, _lines: &[String]) {}
}

/// Run `job` until it ends or reaches a `wait`.
pub fn run(effects: &impl Effects, mut job: Job) -> Ran {
    let mut status = Status::OK;
    let mut printed = Vec::new();
    let mut note = |st: Status, lines: Vec<String>, printed: &mut Vec<String>| {
        if status == Status::OK && st != Status::OK {
            status = st;
        }
        printed.extend(lines);
    };
    while let Some((command, depth)) = job.queue.pop_front() {
        job.ran += 1;
        if job.ran > MAX_COMMANDS {
            let line = format!("stopped: more than {MAX_COMMANDS} commands in one go");
            effects.with_state(|s| s.console_print(line.clone()));
            note(Status::ERROR, vec![line], &mut printed);
            job.queue.clear();
            break;
        }
        let ctx = job.ctx;
        let (mut outcome, lines, paths) = effects.with_state(|s| {
            let mark = s.console_mark();
            let outcome = console::execute(s, &ctx, &command);
            (outcome, s.console_since(mark), s.paths.clone())
        });
        note(outcome.status, lines, &mut printed);
        if let Some(ms) = outcome.wait.take() {
            return Ran {
                status,
                printed,
                rest: (!job.queue.is_empty()).then_some((ms, job)),
            };
        }
        let mut next = std::mem::take(&mut outcome.expand);
        if let Some((name, args)) = outcome.exec.take() {
            match cfg_commands(&paths, &name, &args) {
                Ok(commands) => next = commands,
                Err(line) => {
                    effects.with_state(|s| s.console_print(line.clone()));
                    note(Status::INVALID_ARGUMENT, vec![line], &mut printed);
                }
            }
        }
        if !next.is_empty() {
            if depth + 1 > MAX_DEPTH {
                let line = format!("stopped: aliases and exec nested deeper than {MAX_DEPTH}");
                effects.with_state(|s| s.console_print(line.clone()));
                note(Status::ERROR, vec![line], &mut printed);
            } else {
                job.push_front(next, depth + 1);
            }
        }
        let (st, lines) = effects.perform(outcome);
        note(st, lines, &mut printed);
    }
    Ran {
        status,
        printed,
        rest: None,
    }
}

/// `exec <name> <args>`: the commands of `config/<name>.cfg` when it exists, otherwise the
/// one command `<name> <args>`, so `exec plugin.command arg` runs that command.
fn cfg_commands(paths: &config::Paths, name: &str, args: &str) -> Result<Vec<String>, String> {
    if name.contains(['/', '\\', ':']) || name.contains("..") {
        return Err(format!(
            "exec {name}: names a path; cfg files live in the config folder"
        ));
    }
    let file = paths.cfg_file(name);
    if config::cfg_name(name) {
        match std::fs::read_to_string(&file) {
            Ok(text) => {
                return script::commands(&text).map_err(|e| format!("{}: {e}", file.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", file.display())),
        }
    }
    let command = if args.is_empty() {
        name.to_owned()
    } else {
        format!("{name} {args}")
    };
    Ok(vec![command])
}

#[cfg(test)]
#[path = "run_tests.rs"]
pub(crate) mod tests;
