//! Text forms of the richer setting kinds: colours, dates, times, flag sets and key bindings.
//!
//! Every function takes what a user may type and returns the one canonical spelling the
//! loader stores, or `None` when the text is not of that form. No calendar crate: the
//! checks are small and the forms fixed.

use crate::keys;

/// A colour as `#rrggbb`, or `#rrggbbaa` when it carries alpha; lowercase. Accepts those,
/// `#rgb` and `#rgba`, with or without the `#`.
#[must_use]
pub fn colour(text: &str) -> Option<String> {
    let hex = text.trim();
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let full: String = match hex.len() {
        3 | 4 => hex.chars().flat_map(|c| [c, c]).collect(),
        6 | 8 => hex.to_owned(),
        _ => return None,
    };
    Some(format!("#{}", full.to_ascii_lowercase()))
}

/// The red, green, blue and alpha of a colour in any form [`colour`] takes; alpha is 255
/// when the text has none.
#[must_use]
pub fn colour_rgba(text: &str) -> Option<[u8; 4]> {
    let canonical = colour(text)?;
    let hex = canonical.get(1..)?;
    let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    Some([
        byte(0)?,
        byte(2)?,
        byte(4)?,
        if hex.len() == 8 { byte(6)? } else { 255 },
    ])
}

/// A colour's canonical text from its channels: `#rrggbb` when opaque, else `#rrggbbaa`.
#[must_use]
pub fn colour_text(rgba: [u8; 4]) -> String {
    let [r, g, b, a] = rgba;
    if a == 255 {
        format!("#{r:02x}{g:02x}{b:02x}")
    } else {
        format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
    }
}

/// Days in `month` (1 to 12) of `year` (Gregorian).
#[must_use]
pub fn days_in(year: u32, month: u32) -> u32 {
    match month {
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Numbers separated by `sep`, each of 1 to `widths[i]` digits.
fn numbers<const N: usize>(text: &str, sep: char, widths: [usize; N]) -> Option<[u32; N]> {
    let mut out = [0; N];
    let mut parts = text.split(sep);
    for (slot, width) in out.iter_mut().zip(widths) {
        let part = parts.next()?;
        if part.is_empty() || part.len() > width || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    parts.next().is_none().then_some(out)
}

/// A date as `YYYY-MM-DD`.
#[must_use]
pub fn date(text: &str) -> Option<String> {
    let [y, m, d] = numbers(text.trim(), '-', [4, 2, 2])?;
    let valid =
        (1..=9999).contains(&y) && (1..=12).contains(&m) && (1..=days_in(y, m)).contains(&d);
    valid.then(|| format!("{y:04}-{m:02}-{d:02}"))
}

/// The weekday of a date, Monday 0 to Sunday 6 (Sakamoto's method).
#[must_use]
pub fn weekday(year: u32, month: u32, day: u32) -> u32 {
    const T: [u32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if month < 3 {
        year.saturating_sub(1)
    } else {
        year
    };
    let offset = T
        .get(month.saturating_sub(1) as usize)
        .copied()
        .unwrap_or(0);
    let sunday_first = (y + y / 4 - y / 100 + y / 400 + offset + day) % 7;
    (sunday_first + 6) % 7
}

/// A date's parts, from the canonical form.
#[must_use]
pub fn date_parts(text: &str) -> Option<[u32; 3]> {
    numbers(date(text)?.as_str(), '-', [4, 2, 2])
}

/// A time's hours, minutes and seconds (`None` when it has none), from any form [`time`]
/// takes.
#[must_use]
pub fn time_parts(text: &str) -> Option<(u32, u32, Option<u32>)> {
    let canonical = time(text)?;
    if let Some([h, m, s]) = numbers(&canonical, ':', [2, 2, 2]) {
        return Some((h, m, Some(s)));
    }
    numbers(&canonical, ':', [2, 2]).map(|[h, m]| (h, m, None))
}

/// A time of day as `HH:MM`, or `HH:MM:SS` when seconds are given.
#[must_use]
pub fn time(text: &str) -> Option<String> {
    let text = text.trim();
    if let Some([h, m, s]) = numbers(text, ':', [2, 2, 2]) {
        return (h < 24 && m < 60 && s < 60).then(|| format!("{h:02}:{m:02}:{s:02}"));
    }
    let [h, m] = numbers(text, ':', [2, 2])?;
    (h < 24 && m < 60).then(|| format!("{h:02}:{m:02}"))
}

/// A date and time as `YYYY-MM-DD HH:MM[:SS]`; a `T` between them is accepted too.
#[must_use]
pub fn date_time(text: &str) -> Option<String> {
    let text = text.trim();
    let (d, t) = text.split_once([' ', 'T'])?;
    Some(format!("{} {}", date(d)?, time(t)?))
}

/// A set of `choices`: names separated by `,` or `|`, matched without case, stored in the
/// order of `choices` joined by `,`. Empty for none. `Err` carries the first unknown name.
///
/// # Errors
/// A name that is not one of `choices`.
pub fn flags(text: &str, choices: &[String]) -> Result<String, String> {
    let mut picked = vec![false; choices.len()];
    for name in text
        .split([',', '|'])
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        let at = choices
            .iter()
            .position(|c| c.eq_ignore_ascii_case(name))
            .ok_or_else(|| name.to_owned())?;
        picked[at] = true;
    }
    Ok(choices
        .iter()
        .zip(picked)
        .filter_map(|(c, p)| p.then_some(c.as_str()))
        .collect::<Vec<&str>>()
        .join(","))
}

/// The names in a canonical flag set.
pub fn flag_names(value: &str) -> impl Iterator<Item = &str> {
    value.split(',').map(str::trim).filter(|n| !n.is_empty())
}

/// A key binding list in the hotkey grammar (`f8`, `ctrl+shift+c, sc29`); `none` when
/// unbound.
#[must_use]
pub fn key(text: &str) -> Option<String> {
    keys::parse_list(text).ok().map(|c| keys::format_list(&c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_take_short_long_and_bare_forms() {
        assert_eq!(colour("#ABC").as_deref(), Some("#aabbcc"));
        assert_eq!(colour("ff8000").as_deref(), Some("#ff8000"));
        assert_eq!(colour("#ff800080").as_deref(), Some("#ff800080"));
        assert_eq!(colour("#12345"), None);
        assert_eq!(colour("#gg0000"), None);
        assert_eq!(colour_rgba("#ff8000"), Some([255, 128, 0, 255]));
        assert_eq!(colour_text([255, 128, 0, 255]), "#ff8000");
        assert_eq!(colour_text([0, 0, 0, 128]), "#00000080");
    }

    #[test]
    fn dates_know_month_lengths_and_leap_years() {
        assert_eq!(date("2026-1-9").as_deref(), Some("2026-01-09"));
        assert_eq!(date("2024-02-29").as_deref(), Some("2024-02-29"));
        assert_eq!(date("2026-02-29"), None);
        assert_eq!(date("1900-02-29"), None);
        assert_eq!(date("2000-02-29").as_deref(), Some("2000-02-29"));
        assert_eq!(date("2026-13-01"), None);
        assert_eq!(date("2026-04-31"), None);
        assert_eq!(date("26-04"), None);
        assert_eq!(weekday(2026, 10, 19), 0, "a Monday");
        assert_eq!(weekday(2024, 2, 29), 3, "a Thursday");
        assert_eq!(weekday(2000, 1, 1), 5, "a Saturday");
        assert_eq!(date_parts("2026-1-9"), Some([2026, 1, 9]));
        assert_eq!(time_parts("7:05"), Some((7, 5, None)));
        assert_eq!(time_parts("07:05:09"), Some((7, 5, Some(9))));
    }

    #[test]
    fn times_keep_seconds_only_when_given() {
        assert_eq!(time("7:05").as_deref(), Some("07:05"));
        assert_eq!(time("23:59:59").as_deref(), Some("23:59:59"));
        assert_eq!(time("24:00"), None);
        assert_eq!(time("12:60"), None);
        assert_eq!(
            date_time("2026-10-19T08:30").as_deref(),
            Some("2026-10-19 08:30")
        );
        assert_eq!(date_time("2026-10-19"), None);
    }

    #[test]
    fn flags_follow_choice_order_and_case() {
        let choices: Vec<String> = ["Map", "Chat", "Names"].map(String::from).to_vec();
        assert_eq!(flags("names, map", &choices).as_deref(), Ok("Map,Names"));
        assert_eq!(flags("Chat|chat", &choices).as_deref(), Ok("Chat"));
        assert_eq!(flags("", &choices).as_deref(), Ok(""));
        assert_eq!(flags("map,radio", &choices), Err("radio".to_owned()));
        assert_eq!(
            flag_names("Map,Names").collect::<Vec<_>>(),
            ["Map", "Names"]
        );
    }

    #[test]
    fn keys_use_the_hotkey_grammar() {
        assert_eq!(key("F8").as_deref(), Some("f8"));
        assert_eq!(key("").as_deref(), Some("none"));
        assert_eq!(key("ctrl+nonsense"), None);
    }
}
