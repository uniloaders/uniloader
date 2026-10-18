//! Tab completion for the console input, as a pure function over the candidate names.
//!
//! Kept apart from the drawing so it can be tested without a frame, a device or a game. The
//! candidates come from the loader (`Host::console_names`), so a plugin's settings and
//! commands complete the moment it registers them.

/// What completing a line would do.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Completion {
    /// The line with the completion applied, as far as it is unambiguous.
    pub line: String,
    /// Every candidate that matched, for the hint under the input.
    pub matches: Vec<String>,
}

/// Complete `line` against `names`, or `None` when nothing matches.
///
/// Two passes, because a built-in can be two words (`plugin load`) while a plugin's names are
/// always one: the whole line is tried first, and only if that matches nothing is the last
/// word completed on its own. That is what makes both `plugin lo` and `get vr.ip` work
/// without the caller having to know which kind of line it is holding.
pub(super) fn complete(line: &str, names: &[String]) -> Option<Completion> {
    let typed = line.trim_start();
    if typed.is_empty() {
        return None;
    }
    if let Some(matches) = matching(typed, names) {
        let prefix = line.len() - typed.len();
        return Some(Completion {
            line: format!("{}{}", &line[..prefix], shared(&matches)),
            matches,
        });
    }
    let (head, last) = typed.rsplit_once(char::is_whitespace)?;
    let matches = matching(last, names)?;
    Some(Completion {
        line: format!("{head} {}", shared(&matches)),
        matches,
    })
}

/// Candidates that start with `prefix`, case insensitively, or `None` when there are none.
fn matching(prefix: &str, names: &[String]) -> Option<Vec<String>> {
    let lower = prefix.to_ascii_lowercase();
    let matches: Vec<String> = names
        .iter()
        .filter(|name| name.to_ascii_lowercase().starts_with(&lower))
        .cloned()
        .collect();
    (!matches.is_empty()).then_some(matches)
}

/// The longest prefix every match shares, which is how far the line can be completed.
///
/// With one match that is the whole name; with several it is the part they agree on, so a
/// second press after typing one more character gets further rather than cycling.
fn shared(matches: &[String]) -> String {
    let mut shared = matches.first().cloned().unwrap_or_default();
    for name in matches.iter().skip(1) {
        let common = shared
            .char_indices()
            .zip(name.chars())
            .take_while(|((_, a), b)| a.eq_ignore_ascii_case(b))
            .count();
        shared.truncate(
            shared
                .char_indices()
                .nth(common)
                .map_or(shared.len(), |(at, _)| at),
        );
    }
    shared
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<String> {
        [
            "plugin deps",
            "plugin load",
            "symbols",
            "set",
            "vr.ipd",
            "vr.ipd_scale",
            "vr.recenter",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
    }

    fn complete_to(line: &str) -> Option<String> {
        complete(line, &names()).map(|c| c.line)
    }

    #[test]
    fn one_match_completes_the_whole_word() {
        assert_eq!(complete_to("sym"), Some("symbols".to_owned()));
        assert_eq!(complete_to("vr.r"), Some("vr.recenter".to_owned()));
    }

    #[test]
    fn a_two_word_builtin_completes_as_one_line() {
        assert_eq!(complete_to("plugin lo"), Some("plugin load".to_owned()));
        // Both `plugin deps` and `plugin load` match, so the line gets as far as they agree.
        assert_eq!(complete_to("plugin "), Some("plugin ".to_owned()));
        assert_eq!(
            complete("plugin ", &names()).map(|c| c.matches.len()),
            Some(2)
        );
    }

    #[test]
    fn the_last_word_completes_when_the_line_itself_matches_nothing() {
        assert_eq!(complete_to("get vr.r"), Some("get vr.recenter".to_owned()));
        assert_eq!(
            complete_to("help vr.ipd_"),
            Some("help vr.ipd_scale".to_owned())
        );
    }

    #[test]
    fn several_matches_stop_at_the_shared_prefix() {
        let completion = complete("vr.i", &names()).unwrap_or_else(|| panic!("matches"));
        assert_eq!(completion.line, "vr.ipd");
        assert_eq!(completion.matches, vec!["vr.ipd", "vr.ipd_scale"]);
    }

    #[test]
    fn case_is_ignored_but_the_candidate_spelling_wins() {
        assert_eq!(complete_to("SYM"), Some("symbols".to_owned()));
    }

    #[test]
    fn nothing_to_complete() {
        assert_eq!(complete_to(""), None);
        assert_eq!(complete_to("   "), None);
        assert_eq!(complete_to("zzz"), None);
        assert_eq!(complete_to("set vr.ipd 3"), None);
    }

    #[test]
    fn leading_space_is_kept() {
        assert_eq!(complete_to("  sym"), Some("  symbols".to_owned()));
    }
}
