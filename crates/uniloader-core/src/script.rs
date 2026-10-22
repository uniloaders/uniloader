//! The console's script grammar: commands separated by `;` or newlines, words separated by
//! whitespace, `"..."` grouping, `//` comments.
//!
//! One grammar for everything that runs console text: the console window, the overlay
//! console, cfg files, bind scripts, aliases, `console_exec` and RCON.
//!
//! ```text
//! script   = command { (";" | newline) command }
//! command  = [ word { ws word } ] [ comment ]
//! word     = 1*( bare-char | quoted )
//! quoted   = '"' { char | '\"' | '\\' } '"'
//! comment  = "//" to end of line, outside quotes
//!          | "#" as the first non-blank character of a line
//! ```
//!
//! A deliberate deviation from Source: inside quotes `\"` is a quote and `\\` a backslash, so
//! a bind can hold a quoted argument (`bind f6 "echo \"two words\""`). Any other backslash is
//! literal, so Windows paths need no escaping (`"C:\Games\TSW"`).

use thiserror::Error;

/// Longest script accepted in one submission, in bytes.
pub const MAX_SCRIPT: usize = 64 * 1024;
/// Most commands one submission may run, alias and cfg expansion included.
pub const MAX_COMMANDS: usize = 1000;
/// Deepest alias and `exec` nesting.
pub const MAX_DEPTH: usize = 16;

/// Why a script was rejected.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ScriptError {
    /// A `"` without its closing partner.
    #[error("unterminated quote starting at column {0}")]
    Unterminated(usize),
    /// More than [`MAX_SCRIPT`] bytes.
    #[error("script is {0} bytes; the limit is {MAX_SCRIPT}")]
    TooLong(usize),
}

/// Split a script into its commands, each trimmed, with comments removed and empty commands
/// dropped. Quotes and escapes are kept as written: a command is split into words later, by
/// whoever runs it, so a plugin command still receives its argument text verbatim.
///
/// # Errors
/// An unterminated quote or a script over [`MAX_SCRIPT`].
pub fn commands(script: &str) -> Result<Vec<String>, ScriptError> {
    if script.len() > MAX_SCRIPT {
        return Err(ScriptError::TooLong(script.len()));
    }
    let mut out = Vec::new();
    for line in script.lines() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        split_line(line, &mut out)?;
    }
    Ok(out)
}

/// Split one line at `;` outside quotes, stopping at a `//` comment.
fn split_line(line: &str, out: &mut Vec<String>) -> Result<(), ScriptError> {
    let mut start = 0;
    let mut quote: Option<usize> = None;
    let mut escaped = false;
    let bytes = line.as_bytes();
    let mut end = line.len();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(_open) = quote {
            if escaped {
                escaped = false;
            } else if b == b'\\' && matches!(bytes.get(i + 1), Some(b'"' | b'\\')) {
                escaped = true;
            } else if b == b'"' {
                quote = None;
            }
        } else if b == b'"' {
            quote = Some(i);
        } else if b == b'/' && bytes.get(i + 1) == Some(&b'/') {
            end = i;
            break;
        } else if b == b';' {
            push(out, &line[start..i]);
            start = i + 1;
        }
        i += 1;
    }
    if let Some(open) = quote {
        return Err(ScriptError::Unterminated(line[..open].chars().count() + 1));
    }
    push(out, &line[start..end]);
    Ok(())
}

fn push(out: &mut Vec<String>, command: &str) {
    let command = command.trim();
    if !command.is_empty() {
        out.push(command.to_owned());
    }
}

/// Split one command into words, removing quotes and resolving `\"` and `\\` inside them.
///
/// # Errors
/// An unterminated quote.
pub fn words(command: &str) -> Result<Vec<String>, ScriptError> {
    let mut rest = command;
    let mut out = Vec::new();
    loop {
        match next_word(rest) {
            Ok(Some((word, after))) => {
                out.push(word);
                rest = after;
            }
            Ok(None) => return Ok(out),
            // The column counts from the start of the command, not of the word.
            Err(ScriptError::Unterminated(column)) => {
                let before = command[..command.len() - rest.len()].chars().count();
                return Err(ScriptError::Unterminated(before + column));
            }
            Err(e) => return Err(e),
        }
    }
}

/// The first word of a command (unquoted) and the raw text after it, trimmed. `None` for a
/// command with no words.
///
/// # Errors
/// An unterminated quote in the first word.
pub fn split_first(command: &str) -> Result<Option<(String, &str)>, ScriptError> {
    Ok(next_word(command)?.map(|(word, rest)| (word, rest.trim())))
}

/// Read one word from the start of `text` (after any whitespace). An unterminated quote's
/// column counts from the start of `text`.
fn next_word(text: &str) -> Result<Option<(String, &str)>, ScriptError> {
    let blanks = text.len() - text.trim_start().len();
    let original = text;
    let text = text.trim_start();
    if text.is_empty() {
        return Ok(None);
    }
    let mut word = String::new();
    let mut chars = text.char_indices().peekable();
    let mut quote_at: Option<usize> = None;
    while let Some((i, c)) = chars.next() {
        match (quote_at, c) {
            (None, c) if c.is_whitespace() => return Ok(Some((word, &text[i..]))),
            (None, '"') => quote_at = Some(i),
            (Some(_), '"') => quote_at = None,
            (Some(_), '\\') if matches!(chars.peek(), Some((_, '"' | '\\'))) => {
                if let Some((_, escaped)) = chars.next() {
                    word.push(escaped);
                }
            }
            (_, c) => word.push(c),
        }
    }
    if let Some(open) = quote_at {
        let column = original[..blanks + open].chars().count() + 1;
        return Err(ScriptError::Unterminated(column));
    }
    Ok(Some((word, "")))
}

/// The value a command's raw remainder stands for: the remainder itself, unless it is exactly
/// one quoted word, whose quotes are then removed. `set a.b Hello World` keeps working, and
/// `set a.b "x;y"` sets `x;y`.
#[must_use]
pub fn value(rest: &str) -> String {
    let rest = rest.trim();
    if rest.starts_with('"') {
        if let Ok(Some((word, after))) = next_word(rest) {
            if after.trim().is_empty() {
                return word;
            }
        }
    }
    rest.to_owned()
}

/// Quote a word so [`words`] gives it back unchanged: as is when it needs no quoting,
/// otherwise in double quotes with `"` and `\` escaped.
#[must_use]
pub fn quote(word: &str) -> String {
    let plain = !word.is_empty()
        && !word.contains(|c: char| c.is_whitespace() || c == ';' || c == '"' || c == '\\')
        && !word.contains("//")
        && !word.starts_with('#');
    if plain {
        return word.to_owned();
    }
    let mut out = String::with_capacity(word.len() + 2);
    out.push('"');
    for c in word.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmds(s: &str) -> Vec<String> {
        commands(s).unwrap_or_else(|e| panic!("{s:?}: {e}"))
    }

    fn ws(s: &str) -> Vec<String> {
        words(s).unwrap_or_else(|e| panic!("{s:?}: {e}"))
    }

    #[test]
    fn semicolons_and_newlines_split_outside_quotes_only() {
        assert_eq!(cmds("a; b ;c"), ["a", "b", "c"]);
        assert_eq!(cmds("a\nb\r\nc"), ["a", "b", "c"]);
        assert_eq!(cmds("a;;  ;b"), ["a", "b"], "empty commands are dropped");
        assert_eq!(
            cmds(r#"bind f6 "set a.b x; echo y"; echo z"#),
            [r#"bind f6 "set a.b x; echo y""#, "echo z"]
        );
    }

    #[test]
    fn comments() {
        assert_eq!(cmds("a // note; b"), ["a"]);
        assert_eq!(cmds("# whole line\nb"), ["b"]);
        assert_eq!(cmds("  // only"), Vec::<String>::new());
        assert_eq!(
            cmds(r#"echo "http://x" // real"#),
            [r#"echo "http://x""#],
            "// inside quotes is text"
        );
        assert_eq!(cmds("echo a#b"), ["echo a#b"], "# mid-line is text");
    }

    #[test]
    fn quotes_escapes_and_windows_paths() {
        assert_eq!(
            ws(r#"set x.dir "C:\Games\TSW""#),
            ["set", "x.dir", r"C:\Games\TSW"]
        );
        assert_eq!(
            ws(r#"bind f6 "echo \"two words\"""#),
            ["bind", "f6", r#"echo "two words""#]
        );
        assert_eq!(ws(r#""a\\b""#), [r"a\b"]);
        assert_eq!(ws(r#"pre"fix mid"post"#), ["prefix midpost"]);
        assert_eq!(ws(r#""""#), [""]);
        assert_eq!(ws("   "), Vec::<String>::new());
    }

    #[test]
    fn unterminated_quote_names_its_column() {
        assert_eq!(words(r#"echo "abc"#), Err(ScriptError::Unterminated(6)));
        assert_eq!(
            commands("a\necho \"x; y"),
            Err(ScriptError::Unterminated(6))
        );
        assert_eq!(
            commands(r#"echo "a\""#),
            Err(ScriptError::Unterminated(6)),
            "an escaped quote does not close"
        );
    }

    #[test]
    fn size_limit() {
        let big = "a".repeat(MAX_SCRIPT + 1);
        assert_eq!(commands(&big), Err(ScriptError::TooLong(MAX_SCRIPT + 1)));
    }

    #[test]
    fn the_owners_examples() {
        assert_eq!(
            cmds(r#"bind A "exec plugin.command arg1 arg2""#),
            [r#"bind A "exec plugin.command arg1 arg2""#]
        );
        assert_eq!(
            ws(r#"bind A "exec plugin.command arg1 arg2""#),
            ["bind", "A", "exec plugin.command arg1 arg2"]
        );
        assert_eq!(
            ws(r#"bind CTRL+F9 "set plugin.setting value;reload""#),
            ["bind", "CTRL+F9", "set plugin.setting value;reload"]
        );
        assert_eq!(
            cmds("set plugin.setting value;reload"),
            ["set plugin.setting value", "reload"]
        );
    }

    #[test]
    fn first_word_and_value() {
        assert_eq!(
            split_first("  Coop.Invite  Bob 2 "),
            Ok(Some(("Coop.Invite".to_owned(), "Bob 2")))
        );
        assert_eq!(split_first(""), Ok(None));
        assert_eq!(value("Hello World"), "Hello World");
        assert_eq!(value(r#""x;y""#), "x;y");
        assert_eq!(value(r#""a" b"#), r#""a" b"#);
    }

    #[test]
    fn quoting_round_trips() {
        for word in [
            "",
            "plain",
            "two words",
            r#"say "hi""#,
            r"C:\dir\",
            "a;b",
            "x//y",
            "#hash",
            "tab\there",
        ] {
            let quoted = quote(word);
            assert_eq!(ws(&quoted), [word], "{word:?} quoted as {quoted}");
            assert_eq!(cmds(&format!("echo {quoted}")).len(), 1);
        }
    }
}
