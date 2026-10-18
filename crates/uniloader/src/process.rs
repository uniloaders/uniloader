//! The game process's own command line and environment, read once and shared with plugins.

use std::collections::BTreeMap;

use uniloader_core::cmdline::CommandLine;

/// Loader flags recognised on the game's command line.
pub mod flags {
    /// Spawn a console window that mirrors the loader log and reads typed commands.
    pub const CONSOLE: &str = "loader-console";
    /// Do not load any plugin (forwarding only), overriding `loader.toml`.
    pub const NO_PLUGINS: &str = "noplugins";
    /// Override the log level, for example `--loader-log=debug`.
    pub const LOG_LEVEL: &str = "loader-log";
}

/// Command line and environment of the running process.
#[derive(Debug, Clone)]
pub struct Process {
    /// Parsed command line, without the executable path.
    pub command_line: CommandLine,
    /// Command line verbatim, without the executable path.
    pub raw_command_line: String,
    /// Environment snapshot, sorted by name.
    pub env: BTreeMap<String, String>,
}

impl Process {
    /// Read from the current process.
    #[must_use]
    pub fn current() -> Self {
        let tokens: Vec<String> = std::env::args().skip(1).collect();
        Process {
            raw_command_line: tokens.join(" "),
            command_line: CommandLine::parse(&tokens),
            env: std::env::vars().collect(),
        }
    }

    /// Whether a loader flag is present.
    #[must_use]
    pub fn flag(&self, name: &str) -> bool {
        self.command_line.has(name)
    }

    /// Lines describing the process, for the log header.
    #[must_use]
    pub fn summary(&self) -> Vec<String> {
        let mut lines = vec![format!("command line: {}", self.raw_command_line)];
        for arg in self.command_line.args() {
            let shown = if arg.is_positional() {
                "<positional>".to_owned()
            } else {
                arg.name.clone()
            };
            lines.push(format!(
                "  arg {shown} = {:?}",
                arg.value.as_deref().unwrap_or("")
            ));
        }
        lines.push(format!("{} environment variables", self.env.len()));
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_is_readable() {
        let p = Process::current();
        // The test harness always sets some environment; the command line may be empty.
        assert!(!p.env.is_empty());
        assert!(p
            .summary()
            .iter()
            .any(|l| l.contains("environment variables")));
    }

    #[test]
    fn flags_use_the_shared_grammar() {
        let p = Process {
            raw_command_line: "--loader-console -loader-log=debug".to_owned(),
            command_line: CommandLine::parse(["--loader-console", "-loader-log=debug"]),
            env: BTreeMap::new(),
        };
        assert!(p.flag(flags::CONSOLE));
        assert!(!p.flag(flags::NO_PLUGINS));
        assert_eq!(p.command_line.value(flags::LOG_LEVEL), Some("debug"));
        assert_eq!(p.summary().len(), 1 + 2 + 1);
    }
}
