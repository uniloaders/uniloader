//! What the console shows: one buffer of classified lines, and the queue the logger feeds it
//! through.
//!
//! Command output and log records end up in the same scrollback on purpose — a console that
//! only echoes what was typed cannot answer "what did the loader just do", which is the
//! question people actually open it for. They arrive by different routes, though: command
//! output is printed into the buffer under the state lock, while a log record can be emitted
//! from any thread at any depth, including from inside a state method. So the logger only
//! appends to [`record`]'s queue, which is a leaf lock and takes no other, and the buffer
//! drains that queue at the moments it is already holding the state.
//!
//! Nothing in this module logs. The sink would call straight back into itself.

use std::collections::VecDeque;
use std::sync::Mutex;

/// Log target whose records never reach the scrollback.
///
/// [`crate::state::State::console_print`] logs every console line so the log *file* carries
/// the session's console as well. Routing that back into the console would show every line
/// twice, so those records are written under this target and dropped here.
pub const ECHO_TARGET: &str = "console-echo";

/// How many queued records are kept when nothing is draining them.
///
/// The queue is drained once a frame, so it normally holds a handful. The cap only matters
/// before the first frame and while the game is not presenting at all.
const QUEUE_LIMIT: usize = 4096;

/// Where a line came from, which is what decides its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A line the user typed, echoed back.
    Echo,
    /// Output of a command.
    Output,
    /// A log record, at this level.
    Log(log::Level),
}

impl Kind {
    /// Short tag for the line, for the console's level column.
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            Kind::Echo => "cmd",
            Kind::Output => "out",
            Kind::Log(log::Level::Error) => "ERROR",
            Kind::Log(log::Level::Warn) => "WARN",
            Kind::Log(log::Level::Info) => "INFO",
            Kind::Log(log::Level::Debug) => "DEBUG",
            Kind::Log(log::Level::Trace) => "TRACE",
        }
    }
}

/// One line of console scrollback.
#[derive(Debug, Clone)]
pub struct Line {
    /// Position in the sequence of everything ever printed, for [`crate::state::State::console_since`].
    pub seq: u64,
    /// Where the line came from.
    pub kind: Kind,
    /// Module that logged it, empty for command output.
    pub target: String,
    /// The text itself, with no prefix.
    pub text: String,
}

impl Line {
    /// The line as one string: what the console window, the log file and a capturing plugin
    /// all see when they want it flat.
    #[must_use]
    pub fn flat(&self) -> String {
        match self.kind {
            Kind::Echo | Kind::Output => self.text.clone(),
            Kind::Log(_) => format!("[{}] {}: {}", self.kind.tag(), self.target, self.text),
        }
    }
}

/// Records the logger has produced and nothing has collected yet.
static QUEUE: Mutex<VecDeque<(log::Level, String, String)>> = Mutex::new(VecDeque::new());

fn queue() -> std::sync::MutexGuard<'static, VecDeque<(log::Level, String, String)>> {
    QUEUE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Queue one log record for the console. The logging sink, called from any thread.
pub fn record(level: log::Level, target: &str, message: &str) {
    if target == ECHO_TARGET {
        return;
    }
    let mut guard = queue();
    if guard.len() >= QUEUE_LIMIT {
        guard.pop_front();
    }
    guard.push_back((level, target.to_owned(), message.to_owned()));
}

/// Take everything the logger has queued since the last call.
pub fn take() -> Vec<(log::Level, String, String)> {
    queue().drain(..).collect()
}

/// `HH:MM:SS` of the local clock, for stamping a console line.
///
/// The log file carries full timestamps; the console only needs enough to see how long ago
/// something happened, and a date on every line would eat the width.
#[must_use]
pub fn clock() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let seconds = now % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_echo_target_never_reaches_the_console() {
        // One queue for the process, so this test owns what it puts in and takes it back out.
        let _ = take();
        record(log::Level::Debug, ECHO_TARGET, "> help");
        assert_eq!(take().len(), 0);
    }

    #[test]
    fn a_log_line_is_flattened_with_its_level_and_target() {
        let line = Line {
            seq: 1,
            kind: Kind::Log(log::Level::Warn),
            target: "uniloader::win".to_owned(),
            text: "no plugins directory".to_owned(),
        };
        assert_eq!(line.flat(), "[WARN] uniloader::win: no plugins directory");
        let printed = Line {
            kind: Kind::Output,
            target: String::new(),
            text: "vr.ipd = 1".to_owned(),
            ..line
        };
        assert_eq!(printed.flat(), "vr.ipd = 1");
    }
}
