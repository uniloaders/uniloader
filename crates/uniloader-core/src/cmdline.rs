//! Parsing of the game process command line.
//!
//! Games are launched with a mix of conventions (Unreal's `-key=value`, Steam's launch
//! options, Proton wrappers), so the parser accepts all of them and keeps the original order:
//!
//! | Form | Meaning |
//! | --- | --- |
//! | `--console`, `-console`, `/console` | flag, value `None` |
//! | `--key=value`, `-key=value`, `/key=value` | option with a value |
//! | `--key value` | option with a value, when `value` does not itself look like an option |
//! | `-mod=a;b` | option, as the vanilla game spells it |
//! | `anything-else` | positional argument |
//!
//! Lookups ignore the leading dashes or slash and are case insensitive, so `--Console`,
//! `-console` and `/console` are the same option.

use std::collections::BTreeMap;

/// One command line entry, in the order it appeared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arg {
    /// Normalised name: lowercase, without the leading `-`, `--` or `/`. Empty for positionals.
    pub name: String,
    /// Value, when the entry carried one.
    pub value: Option<String>,
    /// The entry exactly as it appeared, for logging and pass-through.
    pub raw: String,
}

impl Arg {
    /// Whether this entry is a positional argument rather than a flag or option.
    #[must_use]
    pub fn is_positional(&self) -> bool {
        self.name.is_empty()
    }
}

/// A parsed command line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandLine {
    args: Vec<Arg>,
}

fn strip_prefix(token: &str) -> Option<&str> {
    for prefix in ["--", "-", "/"] {
        if let Some(rest) = token.strip_prefix(prefix) {
            // A bare or repeated "-", "--", "/" is not a name, and a negative number is a value.
            if rest.is_empty()
                || rest.starts_with(['-', '/'])
                || rest.starts_with(|c: char| c.is_ascii_digit())
            {
                continue;
            }
            // A slash-prefixed switch is a Windows convention, and a Windows switch name
            // never contains a path separator. Without this, every absolute POSIX path
            // ("/run/media/…") would parse as the switch "run". The value after an "=" is
            // exempt: "/profiles=C:\p" is a switch carrying a path.
            if prefix == "/" {
                let name = rest.split('=').next().unwrap_or(rest);
                if name.contains(['/', '\\']) {
                    continue;
                }
            }
            return Some(rest);
        }
    }
    None
}

impl CommandLine {
    /// Parse a token list. `tokens` must not include the executable path.
    #[must_use]
    pub fn parse<I, S>(tokens: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let tokens: Vec<String> = tokens.into_iter().map(|t| t.as_ref().to_owned()).collect();
        let mut args = Vec::with_capacity(tokens.len());
        let mut index = 0;
        while let Some(token) = tokens.get(index) {
            index += 1;
            let Some(body) = strip_prefix(token) else {
                args.push(Arg {
                    name: String::new(),
                    value: Some(token.clone()),
                    raw: token.clone(),
                });
                continue;
            };
            if let Some((name, value)) = body.split_once('=') {
                args.push(Arg {
                    name: name.to_ascii_lowercase(),
                    value: Some(value.to_owned()),
                    raw: token.clone(),
                });
                continue;
            }
            // `--key value`: only when the next token is not itself an option.
            let follows = tokens
                .get(index)
                .filter(|next| strip_prefix(next).is_none());
            let value = follows.cloned();
            let raw = match &value {
                Some(v) => {
                    index += 1;
                    format!("{token} {v}")
                }
                None => token.clone(),
            };
            args.push(Arg {
                name: body.to_ascii_lowercase(),
                value,
                raw,
            });
        }
        CommandLine { args }
    }

    /// Every entry in command line order.
    #[must_use]
    pub fn args(&self) -> &[Arg] {
        &self.args
    }

    /// Whether an option or flag with this name is present, however it was spelled.
    #[must_use]
    pub fn has(&self, name: &str) -> bool {
        self.find(name).is_some()
    }

    /// First entry with this name.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&Arg> {
        let wanted = normalise(name);
        self.args.iter().find(|a| a.name == wanted)
    }

    /// Value of the first entry with this name, when it carried one.
    #[must_use]
    pub fn value(&self, name: &str) -> Option<&str> {
        self.find(name)?.value.as_deref()
    }

    /// Value parsed as `T`, or `None` when absent or unparsable.
    #[must_use]
    pub fn value_as<T: std::str::FromStr>(&self, name: &str) -> Option<T> {
        self.value(name)?.parse().ok()
    }

    /// Positional arguments in order.
    pub fn positionals(&self) -> impl Iterator<Item = &str> {
        self.args
            .iter()
            .filter(|a| a.is_positional())
            .filter_map(|a| a.value.as_deref())
    }

    /// Named entries as a map. A repeated name keeps its first value, matching [`Self::value`].
    #[must_use]
    pub fn to_map(&self) -> BTreeMap<String, Option<String>> {
        let mut map = BTreeMap::new();
        for arg in self.args.iter().filter(|a| !a.is_positional()) {
            map.entry(arg.name.clone())
                .or_insert_with(|| arg.value.clone());
        }
        map
    }
}

fn normalise(name: &str) -> String {
    strip_prefix(name).unwrap_or(name).to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> CommandLine {
        CommandLine::parse(line.split_whitespace())
    }

    #[test]
    fn accepts_every_prefix_and_separator() {
        let cmd = parse("--console -name=Blu /window --profiles=C:\\p");
        assert!(cmd.has("console"));
        assert!(cmd.has("--console"), "lookups normalise the prefix");
        assert!(cmd.has("/CONSOLE"));
        assert_eq!(cmd.value("name"), Some("Blu"));
        assert_eq!(cmd.find("window").and_then(|a| a.value.as_deref()), None);
        assert_eq!(cmd.value("profiles"), Some("C:\\p"));
        assert!(!cmd.has("missing"));
    }

    #[test]
    fn space_separated_values_stop_at_the_next_option() {
        let cmd = parse("--mission dayzOffline --console --port 2302");
        assert_eq!(cmd.value("mission"), Some("dayzOffline"));
        assert_eq!(cmd.find("console").and_then(|a| a.value.clone()), None);
        assert_eq!(cmd.value_as::<u16>("port"), Some(2302));
        assert_eq!(cmd.value_as::<u16>("mission"), None);
    }

    #[test]
    fn positionals_and_vanilla_mod_lists() {
        let cmd = parse("DayZ_x64.exe -mod=@CF;@VPP -newui");
        assert_eq!(cmd.positionals().collect::<Vec<_>>(), vec!["DayZ_x64.exe"]);
        assert_eq!(cmd.value("mod"), Some("@CF;@VPP"));
        assert!(cmd.has("newui"));
    }

    #[test]
    fn negative_numbers_and_bare_dashes_are_values() {
        let cmd = parse("--offset -1.5 -- -");
        assert_eq!(cmd.value("offset"), Some("-1.5"));
        assert_eq!(cmd.positionals().collect::<Vec<_>>(), vec!["--", "-"]);
    }

    #[test]
    fn absolute_posix_paths_are_values_not_switches() {
        // Regression: "/run/media/..." parsed as the switch "run", which swallowed the
        // value of the option in front of it.
        let cmd = parse("--exe /run/media/Data/DayZ_x64.exe --seed ./seed.json");
        assert_eq!(cmd.value("exe"), Some("/run/media/Data/DayZ_x64.exe"));
        assert_eq!(cmd.value("seed"), Some("./seed.json"));
        assert!(!cmd.has("run"));
    }

    #[test]
    fn slash_switches_still_work_and_may_carry_paths() {
        let cmd = parse(r"/console /profiles=C:\Users\p /mod=@CF");
        assert!(cmd.has("console"));
        assert_eq!(cmd.value("profiles"), Some(r"C:\Users\p"));
        assert_eq!(cmd.value("mod"), Some("@CF"));
    }

    #[test]
    fn map_keeps_first_value_and_order_is_preserved() {
        let cmd = parse("--a=1 --b --a=2");
        let map = cmd.to_map();
        assert_eq!(map["a"], Some("1".to_owned()));
        assert_eq!(map["b"], None);
        assert_eq!(
            cmd.args()
                .iter()
                .map(|a| a.raw.as_str())
                .collect::<Vec<_>>(),
            vec!["--a=1", "--b", "--a=2"]
        );
    }
}
