//! Target filters: which game a plugin is meant for, and whether the running one is it.
//!
//! A plugin declares filters, each a property name and a list of accepted values. The loader
//! reads the running game's properties ([`Facts`]) and every declared filter must accept the
//! game's value for its property. A property the loader could not read fails its filter,
//! because "meant for this game" cannot be confirmed then.
//!
//! How a value is matched depends on what the property holds:
//!
//! - **Text** (names, `pe.version.*` strings, hashes): case-insensitive, with `*` for any run
//!   of characters and `?` for one, so `Train Sim World*` accepts every edition. A value
//!   starting with a comparator (`>=1.2`) compares the version the text starts with instead,
//!   for version strings such as `pe.version.FileVersion` or `unity.version`.
//! - **Versions** (`pe.fixed.file_version`, `unreal.version`, ...): a bare version accepts that
//!   version and everything under it (`4.26` accepts `4.26.2.0`); comparators separated by
//!   spaces or commas must all hold (`>=4.26 <5`, `>1.0, <=2.3.4`). Missing parts count as 0.
//! - **Numbers** (`pe.timestamp`, ...): decimal or `0x` hex, exact or with comparators like
//!   versions; `&0x20` accepts when all those bits are set (flags).
//!
//! Property names are matched ignoring case.

use std::cmp::Ordering;
use std::collections::BTreeMap;

/// One filter a plugin declared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    /// Property name, for example `exe.name`.
    pub property: String,
    /// Accepted values; any one matches.
    pub values: Vec<String>,
}

/// One property of the running game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fact {
    /// Text, matched by pattern.
    Text(String),
    /// A dotted version, matched by requirement.
    Version(Vec<u64>),
    /// A number, matched exactly or by comparison.
    Number(u64),
}

impl std::fmt::Display for Fact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fact::Text(text) => f.write_str(text),
            Fact::Version(parts) => {
                let parts: Vec<String> = parts.iter().map(u64::to_string).collect();
                f.write_str(&parts.join("."))
            }
            Fact::Number(n) => write!(f, "{n}"),
        }
    }
}

/// Every property the loader could read, by name.
pub type Facts = BTreeMap<String, Fact>;

/// Parse a dotted version: leading `v`, then numbers separated by dots; anything after the
/// numbers (`-beta`, `+build`, a space) ends it. `None` when it starts with no number.
#[must_use]
pub fn version(text: &str) -> Option<Vec<u64>> {
    let text = text.trim();
    let text = text
        .strip_prefix(['v', 'V'])
        .filter(|t| t.starts_with(|c: char| c.is_ascii_digit()))
        .unwrap_or(text);
    let mut parts = Vec::new();
    for part in text.split('.') {
        let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            break;
        }
        parts.push(digits.parse().ok()?);
        if digits.len() != part.len() {
            break;
        }
    }
    (!parts.is_empty()).then_some(parts)
}

/// Compare versions, missing parts counting as 0.
#[must_use]
pub fn compare(a: &[u64], b: &[u64]) -> Ordering {
    let len = a.len().max(b.len());
    (0..len)
        .map(|i| {
            let x = a.get(i).copied().unwrap_or(0);
            let y = b.get(i).copied().unwrap_or(0);
            x.cmp(&y)
        })
        .find(|o| o.is_ne())
        .unwrap_or(Ordering::Equal)
}

/// Whether `pattern` (`*`, `?`) matches `text`, ignoring ASCII case.
#[must_use]
pub fn glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    // Iterative matching with one backtrack point, the usual way for `*`.
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        match p.get(pi) {
            Some('*') => {
                star = Some((pi, ti));
                pi += 1;
            }
            Some(c) if *c == '?' || *c == t[ti] => {
                pi += 1;
                ti += 1;
            }
            _ => match star {
                Some((sp, st)) => {
                    pi = sp + 1;
                    ti = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// Whether a version requirement (`4.26`, `>=4.26 <5`) accepts `have`.
fn version_accepts(requirement: &str, have: &[u64]) -> bool {
    let parts: Vec<&str> = requirement
        .split([' ', ','])
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        return false;
    }
    parts.iter().all(|part| {
        let (op, rest) = ["<=", ">=", "==", "=", "<", ">"]
            .iter()
            .find_map(|op| part.strip_prefix(op).map(|rest| (*op, rest)))
            .unwrap_or(("", part));
        let Some(want) = version(rest) else {
            return false;
        };
        let order = compare(have, &want);
        match op {
            "<=" => order.is_le(),
            ">=" => order.is_ge(),
            "<" => order.is_lt(),
            ">" => order.is_gt(),
            "=" | "==" => order.is_eq(),
            // Bare: that version and everything under it.
            _ => have.len() >= want.len() && have[..want.len()] == want[..],
        }
    })
}

fn number(text: &str) -> Option<u64> {
    let text = text.trim();
    match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

/// Whether `value` starts with a comparator.
fn is_comparison(value: &str) -> bool {
    value.starts_with(['<', '>', '='])
}

/// Whether `value` (one accepted value of a filter) accepts `fact`.
#[must_use]
pub fn accepts(value: &str, fact: &Fact) -> bool {
    let value = value.trim();
    match fact {
        Fact::Text(text) if is_comparison(value) => {
            version(text).is_some_and(|have| version_accepts(value, &have))
        }
        Fact::Text(text) => glob(value, text),
        Fact::Version(have) => version_accepts(value, have),
        Fact::Number(have) => {
            if let Some(mask) = value.strip_prefix('&').and_then(number) {
                return have & mask == mask;
            }
            match number(value) {
                Some(want) => *have == want,
                None => version_accepts(value, &[*have]),
            }
        }
    }
}

/// The fact named `property`, ignoring case.
#[must_use]
pub fn lookup<'a>(facts: &'a Facts, property: &str) -> Option<&'a Fact> {
    facts.get(property).or_else(|| {
        facts
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(property))
            .map(|(_, fact)| fact)
    })
}

/// The filters the facts fail, each as a line saying why. Empty when everything matches.
#[must_use]
pub fn mismatches(filters: &[Filter], facts: &Facts) -> Vec<String> {
    filters
        .iter()
        .filter_map(|filter| {
            let wanted = filter.values.join(" | ");
            match lookup(facts, &filter.property) {
                None => Some(format!(
                    "{} wants {wanted}, but the game has no such property",
                    filter.property
                )),
                Some(fact) if filter.values.iter().any(|v| accepts(v, fact)) => None,
                Some(fact) => Some(format!(
                    "{} is {fact}, the plugin wants {wanted}",
                    filter.property
                )),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Fact {
        Fact::Text(s.to_owned())
    }

    #[test]
    fn globs() {
        assert!(glob("Train Sim World*", "Train Sim World 7"));
        assert!(glob(
            "ts2prototype-win64-shipping.exe",
            "TS2Prototype-Win64-Shipping.exe"
        ));
        assert!(glob("*World ?", "Train Sim World 5"));
        assert!(!glob("*World ?", "Train Sim World 10"));
        assert!(glob("*", ""));
        assert!(!glob("a*b", "acd"));
        assert!(glob("a*b*c", "a-b-b-c"));
    }

    #[test]
    fn versions() {
        assert_eq!(version("v4.26.2-beta"), Some(vec![4, 26, 2]));
        assert_eq!(version("1.2.3.4"), Some(vec![1, 2, 3, 4]));
        assert_eq!(version("x1"), None);
        let have = Fact::Version(vec![4, 26, 2, 0]);
        assert!(accepts("4.26", &have));
        assert!(!accepts("4.2", &have), "a bare version is a prefix by part");
        assert!(accepts(">=4.26 <5", &have));
        assert!(accepts(">=4.25, <=4.26.2", &have));
        assert!(!accepts(">4.26.2", &have));
        assert!(accepts("=4.26.2", &have));
        assert!(!accepts("", &have));
        assert_eq!(compare(&[1, 0], &[1]), Ordering::Equal);
    }

    #[test]
    fn numbers() {
        let stamp = Fact::Number(0x6512_0000);
        assert!(accepts("0x65120000", &stamp));
        assert!(accepts(">=1695678464", &stamp));
        assert!(!accepts("1", &stamp));
        let flags = Fact::Number(0x8160);
        assert!(accepts("&0x8100", &flags));
        assert!(!accepts("&0x1", &flags));
    }

    #[test]
    fn text_compares_as_a_version_after_a_comparator() {
        let unity = text("2021.3.16f1");
        assert!(accepts(">=2021.3 <2022", &unity));
        assert!(!accepts(">=2022", &unity));
        assert!(accepts("2021.3.*", &unity));
        assert!(!accepts(">1", &text("no version")));
    }

    #[test]
    fn every_filter_must_match_and_any_value_of_one_does() {
        let facts: Facts = [
            ("exe.name".to_owned(), text("TrainSimWorld.exe")),
            ("pe.version.CompanyName".to_owned(), text("Dovetail Games")),
            (
                "pe.version.040904b0.ProductName".to_owned(),
                text("TrainSimWorld7"),
            ),
        ]
        .into_iter()
        .collect();
        let filter = |property: &str, values: &[&str]| Filter {
            property: property.to_owned(),
            values: values.iter().map(|v| (*v).to_owned()).collect(),
        };
        let ok = [
            filter("exe.name", &["TS2Prototype*.exe", "TrainSimWorld.exe"]),
            filter("pe.version.companyname", &["dovetail*"]),
            filter("pe.version.040904B0.ProductName", &["TrainSimWorld?"]),
        ];
        assert_eq!(mismatches(&ok, &facts), Vec::<String>::new());
        let bad = [
            filter("exe.name", &["DayZ_x64.exe"]),
            filter("unreal.version", &["4.26"]),
        ];
        let why = mismatches(&bad, &facts);
        assert_eq!(why.len(), 2);
        assert!(why[0].contains("TrainSimWorld.exe"), "{why:?}");
        assert!(why[1].contains("no such property"), "{why:?}");
    }
}
