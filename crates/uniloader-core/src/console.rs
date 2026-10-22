//! Console line parsing and the built-in command set.
//!
//! The console owns no state of its own; the loader resolves names against the plugin,
//! setting and command registries. This module only decides *what* a typed line means.
//! Lines reach the loader through its command file and the dev API rather than a window.

use thiserror::Error;

use crate::script::{self, ScriptError};

/// What `plugin <op>` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginOp {
    /// `plugin list`: loaded plugins with their state.
    List,
    /// `plugin deps [name]`: declared dependencies.
    Deps,
    /// `plugin load <name>`: load and start a DLL from the plugin directory.
    Load,
    /// `plugin stop <name>`: call the plugin's stop export and silence it.
    Stop,
    /// `plugin enable <name>`: resume callback delivery.
    Enable,
    /// `plugin disable <name>`: pause callback delivery without stopping the plugin.
    Disable,
    /// `plugin reload <name>`: not possible in-process; the loader explains why.
    Reload,
}

impl PluginOp {
    /// Whether the operation acts on one named plugin.
    #[must_use]
    pub fn needs_name(self) -> bool {
        !matches!(self, PluginOp::List | PluginOp::Deps)
    }

    /// Canonical spelling of the operation.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            PluginOp::List => "list",
            PluginOp::Deps => "deps",
            PluginOp::Load => "load",
            PluginOp::Stop => "stop",
            PluginOp::Enable => "enable",
            PluginOp::Disable => "disable",
            PluginOp::Reload => "reload",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "list" | "ls" => PluginOp::List,
            "deps" | "dependencies" => PluginOp::Deps,
            "load" => PluginOp::Load,
            "stop" | "unload" => PluginOp::Stop,
            "enable" => PluginOp::Enable,
            "disable" => PluginOp::Disable,
            "reload" => PluginOp::Reload,
            _ => return None,
        })
    }
}

/// A parsed console line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// Nothing typed (blank or comment).
    Empty,
    /// `help [name]`.
    Help(Option<String>),
    /// `list [prefix]`: settings and commands, optionally filtered by prefix.
    List(Option<String>),
    /// `get <plugin.key>`.
    Get(String),
    /// `set <plugin.key> <value>`.
    Set(String, String),
    /// `plugins`, or `plugin <op> [name]`: the plugin lifecycle commands.
    Plugin(PluginOp, Option<String>),
    /// `hotkeys`: every hotkey action and what it is bound to.
    Hotkeys,
    /// `hooks`: patches, vtable slots and detours plugins installed through the loader.
    Hooks,
    /// `<plugin.key>` alone prints the value, `<plugin.key> <value>` sets it.
    Variable(String, Option<String>),
    /// `<plugin.command> [args...]`: forwarded to the owning plugin with raw args.
    Command(String, String),
    /// `quit`: close the game the way its own menu would.
    Quit,
}

/// Why a line could not be parsed.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    /// `set` without a value.
    #[error("usage: set <plugin.key> <value>")]
    SetNeedsValue,
    /// `get`/`set` without a name.
    #[error("usage: {0} <plugin.key>")]
    NeedsName(&'static str),
    /// `plugin` with no operation, or one that does not exist.
    #[error("usage: plugin <list|deps|load|stop|enable|disable|reload> [name]")]
    PluginUsage,
    /// A `plugin` operation that acts on one plugin, without its name.
    #[error("usage: plugin {0} <name>")]
    PluginNeedsName(&'static str),
    /// The command's quoting is broken.
    #[error(transparent)]
    Script(#[from] ScriptError),
}

/// Split the first whitespace separated word from the rest.
fn split_word(text: &str) -> (&str, &str) {
    let text = text.trim_start();
    match text.find(char::is_whitespace) {
        Some(i) => (&text[..i], text[i..].trim()),
        None => (text, ""),
    }
}

/// Parse one command, as [`script::commands`] split it from a script. Identifiers are
/// lower-cased; values and arguments keep their case.
///
/// # Errors
/// See [`ParseError`].
pub fn parse(line: &str) -> Result<Line, ParseError> {
    let text = line.trim();
    if text.is_empty() || text.starts_with('#') || text.starts_with("//") {
        return Ok(Line::Empty);
    }
    let Some((word, rest)) = script::split_first(text)? else {
        return Ok(Line::Empty);
    };
    let word = word.to_ascii_lowercase();
    let optional = |s: &str| (!s.is_empty()).then(|| s.to_ascii_lowercase());
    Ok(match word.as_str() {
        "help" | "?" => Line::Help(optional(rest)),
        "list" | "ls" => Line::List(optional(rest)),
        "plugins" => Line::Plugin(PluginOp::List, None),
        "plugin" => {
            let (op, name) = split_word(rest);
            let op = PluginOp::parse(&op.to_ascii_lowercase()).ok_or(ParseError::PluginUsage)?;
            // Plugin names are lower case by rule, so normalising keeps `plugin stop Jump`
            // working the way the rest of the console does.
            let name = optional(name);
            if op.needs_name() && name.is_none() {
                return Err(ParseError::PluginNeedsName(op.as_str()));
            }
            Line::Plugin(op, name)
        }
        "hotkeys" | "keys" => Line::Hotkeys,
        "hooks" => Line::Hooks,
        "quit" | "exit" => Line::Quit,
        "get" => {
            if rest.is_empty() {
                return Err(ParseError::NeedsName("get"));
            }
            Line::Get(rest.to_ascii_lowercase())
        }
        "set" => {
            let (name, value) = split_word(rest);
            if name.is_empty() {
                return Err(ParseError::NeedsName("set"));
            }
            if value.is_empty() {
                return Err(ParseError::SetNeedsValue);
            }
            Line::Set(name.to_ascii_lowercase(), script::value(value))
        }
        _ if word.contains('.') => Line::Command(word, rest.to_owned()),
        _ => Line::Variable(word, optional(rest).map(|_| script::value(rest))),
    })
}

/// Every built-in word a console line can start with, plus the `plugin` sub-commands, for
/// completion. Kept in the same module as [`parse`] so the two cannot drift apart; the test
/// below asserts that each one is recognised by the grammar.
pub const BUILTIN_NAMES: &[&str] = &[
    "help",
    "list",
    "plugins",
    "plugin deps",
    "plugin load",
    "plugin stop",
    "plugin enable",
    "plugin disable",
    "plugin list",
    "hotkeys",
    "hooks",
    "get",
    "set",
    "quit",
];

/// Help text for the built-in commands, one entry per line.
pub const BUILTIN_HELP: &[(&str, &str)] = &[
    (
        "help [name]",
        "Show this list, or help for one setting or command.",
    ),
    (
        "list [prefix]",
        "List settings and commands, optionally filtered.",
    ),
    ("plugins", "List loaded plugins and their state."),
    (
        "plugin deps [name]",
        "Show what plugins declared they need.",
    ),
    (
        "plugin load <name>",
        "Load and start a DLL from the plugin directory.",
    ),
    (
        "plugin stop <name>",
        "Stop a plugin; the DLL stays in the process.",
    ),
    (
        "plugin enable|disable <name>",
        "Resume or pause callback delivery.",
    ),
    ("hotkeys", "List every hotkey action and its binding."),
    (
        "hooks",
        "List the hooks plugins installed through the loader.",
    ),
    ("quit", "Close the game, the way its own menu would."),
    ("get <plugin.key>", "Print a setting."),
    ("set <plugin.key> <value>", "Change a setting."),
    ("<plugin.key> [value]", "Shorthand for get / set."),
    ("<plugin.command> [args]", "Run a plugin command."),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_and_comments_are_empty() {
        for l in ["", "   ", "# note", "// note"] {
            assert_eq!(parse(l), Ok(Line::Empty));
        }
    }

    #[test]
    fn builtins() {
        assert_eq!(parse("HELP"), Ok(Line::Help(None)));
        assert_eq!(
            parse("help tsw-coop.relay"),
            Ok(Line::Help(Some("tsw-coop.relay".into())))
        );
        assert_eq!(parse("ls jump"), Ok(Line::List(Some("jump".into()))));
        assert_eq!(parse("plugins"), Ok(Line::Plugin(PluginOp::List, None)));
        assert_eq!(parse("hotkeys"), Ok(Line::Hotkeys));
        assert_eq!(parse("hooks"), Ok(Line::Hooks));
        assert_eq!(parse("get A.B"), Ok(Line::Get("a.b".into())));
        assert_eq!(
            parse("set a.b Hello World"),
            Ok(Line::Set("a.b".into(), "Hello World".into()))
        );
        assert_eq!(
            parse(r#"set a.b "x;y""#),
            Ok(Line::Set("a.b".into(), "x;y".into())),
            "one quoted word loses its quotes"
        );
        assert_eq!(
            parse(r#""SET" a.b 1"#),
            Ok(Line::Set("a.b".into(), "1".into()))
        );
        assert!(matches!(parse(r#""echo x"#), Err(ParseError::Script(_))));
        assert_eq!(parse("set a.b"), Err(ParseError::SetNeedsValue));
        assert_eq!(parse("get"), Err(ParseError::NeedsName("get")));
        assert_eq!(parse("EXIT"), Ok(Line::Quit));
    }

    #[test]
    fn plugin_operations() {
        assert_eq!(parse("plugin ls"), Ok(Line::Plugin(PluginOp::List, None)));
        assert_eq!(
            parse("plugin deps"),
            Ok(Line::Plugin(PluginOp::Deps, None)),
            "deps without a name shows every plugin"
        );
        assert_eq!(
            parse("plugin load Jump"),
            Ok(Line::Plugin(PluginOp::Load, Some("jump".into())))
        );
        assert_eq!(
            parse("plugin unload jump"),
            Ok(Line::Plugin(PluginOp::Stop, Some("jump".into()))),
            "unload is a synonym for stop, which is what it really does"
        );
        assert_eq!(parse("plugin"), Err(ParseError::PluginUsage));
        assert_eq!(parse("plugin frobnicate x"), Err(ParseError::PluginUsage));
        assert_eq!(
            parse("plugin stop"),
            Err(ParseError::PluginNeedsName("stop"))
        );
    }

    #[test]
    fn every_completion_candidate_is_a_word_the_grammar_knows() {
        for name in BUILTIN_NAMES {
            // A rejection is a pass: it means the word was recognised and its arguments were
            // not supplied, which is exactly what completing a bare command leaves behind.
            // Falling through to a variable or a plugin command is the failure, because that
            // is what the grammar does with a word it has never heard of.
            if let Ok(parsed) = parse(name) {
                assert!(
                    !matches!(parsed, Line::Variable(..) | Line::Command(..)),
                    "{name} is offered for completion but the grammar does not know it: {parsed:?}"
                );
            }
        }
    }

    #[test]
    fn dotted_words_are_commands_or_variables() {
        assert_eq!(
            parse("jump.now"),
            Ok(Line::Command("jump.now".into(), String::new()))
        );
        assert_eq!(
            parse("Coop.Invite Bob 2"),
            Ok(Line::Command("coop.invite".into(), "Bob 2".into()))
        );
        assert_eq!(parse("fps"), Ok(Line::Variable("fps".into(), None)));
        assert_eq!(
            parse("fps 60"),
            Ok(Line::Variable("fps".into(), Some("60".into())))
        );
    }
}
