//! Plugin dependency grammar and load ordering.
//!
//! A plugin declares what it needs in its describe export. Requirements on other plugins are
//! the only kind this module knows about, because they are the only kind that decides *order*;
//! the platform layer checks libraries, files and symbols before calling [`plan`] and leaves a
//! plugin out when one of those is missing.
//!
//! The planner is total: it never fails as a whole. It returns the plugins that can start, in
//! an order where every dependency starts first, plus one rejection per plugin that cannot,
//! with the reason. One broken plugin therefore never blocks a launch.

use std::collections::BTreeMap;

use thiserror::Error;

/// Comparison in a version requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    /// `=`
    Exactly,
    /// `>`
    Greater,
    /// `>=`, and what a bare version means.
    AtLeast,
    /// `<`
    Less,
    /// `<=`
    AtMost,
}

/// Why a version requirement could not be parsed.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ReqError {
    /// A comparator had no version after it.
    #[error("missing version after {0:?}")]
    MissingVersion(String),
}

/// A dot separated version, compared component by component.
///
/// Numeric components compare as numbers, so `1.10` is newer than `1.9`; a component that is
/// not a number compares as text. A missing component counts as zero, so `1.2` equals `1.2.0`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Version(Vec<String>);

impl Version {
    fn parse(text: &str) -> Self {
        Version(
            text.trim()
                .split('.')
                .map(|p| p.trim().to_ascii_lowercase())
                .collect(),
        )
    }

    fn compare(&self, other: &Version) -> core::cmp::Ordering {
        use core::cmp::Ordering;
        let len = self.0.len().max(other.0.len());
        for i in 0..len {
            let zero = String::from("0");
            let a = self.0.get(i).unwrap_or(&zero);
            let b = other.0.get(i).unwrap_or(&zero);
            let ordering = match (a.parse::<u64>(), b.parse::<u64>()) {
                (Ok(a), Ok(b)) => a.cmp(&b),
                _ => a.cmp(b),
            };
            if ordering != Ordering::Equal {
                return ordering;
            }
        }
        Ordering::Equal
    }
}

/// A parsed version requirement: comma separated comparators that must all hold.
///
/// `>=1.2`, `>=1.2, <2`, `=1.4.0`, or a bare `1.2`, which means `>=1.2`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionReq {
    parts: Vec<(Op, Version)>,
    text: String,
}

impl VersionReq {
    /// Parse a requirement. An empty string accepts every version.
    ///
    /// # Errors
    /// See [`ReqError`].
    pub fn parse(text: &str) -> Result<Self, ReqError> {
        let mut parts = Vec::new();
        for term in text.split(',').map(str::trim).filter(|t| !t.is_empty()) {
            let (op, rest) = if let Some(rest) = term.strip_prefix(">=") {
                (Op::AtLeast, rest)
            } else if let Some(rest) = term.strip_prefix("<=") {
                (Op::AtMost, rest)
            } else if let Some(rest) = term.strip_prefix("==") {
                (Op::Exactly, rest)
            } else if let Some(rest) = term.strip_prefix('>') {
                (Op::Greater, rest)
            } else if let Some(rest) = term.strip_prefix('<') {
                (Op::Less, rest)
            } else if let Some(rest) = term.strip_prefix('=') {
                (Op::Exactly, rest)
            } else {
                (Op::AtLeast, term)
            };
            let rest = rest.trim();
            if rest.is_empty() {
                return Err(ReqError::MissingVersion(term.to_owned()));
            }
            parts.push((op, Version::parse(rest)));
        }
        Ok(VersionReq {
            parts,
            text: text.trim().to_owned(),
        })
    }

    /// Whether `version` satisfies every comparator.
    #[must_use]
    pub fn matches(&self, version: &str) -> bool {
        use core::cmp::Ordering::{Equal, Greater, Less};
        let have = Version::parse(version);
        self.parts.iter().all(|(op, want)| {
            let ordering = have.compare(want);
            match op {
                Op::Exactly => ordering == Equal,
                Op::Greater => ordering == Greater,
                Op::AtLeast => ordering != Less,
                Op::Less => ordering == Less,
                Op::AtMost => ordering != Greater,
            }
        })
    }

    /// Whether the requirement accepts anything.
    #[must_use]
    pub fn is_any(&self) -> bool {
        self.parts.is_empty()
    }

    /// The requirement as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }
}

/// A requirement on another plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginReq {
    /// Name of the required plugin.
    pub name: String,
    /// Accepted versions.
    pub version: VersionReq,
    /// Carry on without it; the requirement then only affects load order.
    pub optional: bool,
}

/// A plugin offered to [`plan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Plugin name from its describe export.
    pub name: String,
    /// Plugin version from its describe export.
    pub version: String,
    /// Requirements on other plugins, in declaration order.
    pub requires: Vec<PluginReq>,
}

/// Why a candidate cannot start.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum Rejection {
    /// A required plugin is not among the candidates.
    #[error("requires plugin {0}, which is not loaded")]
    Missing(String),
    /// A required plugin is present with a version the requirement does not accept.
    #[error("requires plugin {name} {req}, but {version} is loaded")]
    Version {
        /// Required plugin.
        name: String,
        /// Requirement as written.
        req: String,
        /// Version actually present.
        version: String,
    },
    /// A required plugin was itself rejected.
    #[error("requires plugin {0}, which did not start")]
    DependencyRejected(String),
    /// The plugin takes part in a dependency cycle.
    #[error("dependency cycle: {0}")]
    Cycle(String),
    /// Two candidates declare the same name.
    #[error("another plugin is already named {0}")]
    DuplicateName(String),
}

/// Result of planning: who starts, in what order, and who does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Indices into the candidate slice, in start order.
    pub order: Vec<usize>,
    /// Candidate index and why it was left out, in candidate order.
    pub rejected: Vec<(usize, Rejection)>,
}

/// Order the candidates so every dependency starts before its dependants.
///
/// Candidates that cannot start are reported in `rejected` rather than aborting the plan.
/// Within one dependency level the input order is preserved, so the result is reproducible.
#[must_use]
pub fn plan(candidates: &[Candidate]) -> Plan {
    let mut by_name: BTreeMap<&str, usize> = BTreeMap::new();
    let mut rejected: BTreeMap<usize, Rejection> = BTreeMap::new();
    for (i, candidate) in candidates.iter().enumerate() {
        if by_name.contains_key(candidate.name.as_str()) {
            rejected.insert(i, Rejection::DuplicateName(candidate.name.clone()));
        } else {
            by_name.insert(&candidate.name, i);
        }
    }

    // Direct requirement failures first: a missing or mismatched plugin is a property of one
    // candidate alone, and rejecting it up front lets the sweep below propagate it.
    for (i, candidate) in candidates.iter().enumerate() {
        if rejected.contains_key(&i) {
            continue;
        }
        for req in &candidate.requires {
            let found = by_name.get(req.name.as_str()).map(|&j| &candidates[j]);
            let failure = match found {
                None if req.optional => None,
                None => Some(Rejection::Missing(req.name.clone())),
                Some(other) if !req.version.matches(&other.version) => Some(Rejection::Version {
                    name: req.name.clone(),
                    req: req.version.as_str().to_owned(),
                    version: other.version.clone(),
                }),
                Some(_) => None,
            };
            if let Some(failure) = failure {
                rejected.insert(i, failure);
                break;
            }
        }
    }

    propagate(candidates, &by_name, &mut rejected);
    let order = toposort(candidates, &by_name, &mut rejected);
    Plan {
        order,
        rejected: rejected.into_iter().collect(),
    }
}

/// Reject, transitively, everything that requires something already rejected.
fn propagate(
    candidates: &[Candidate],
    by_name: &BTreeMap<&str, usize>,
    rejected: &mut BTreeMap<usize, Rejection>,
) {
    loop {
        let mut added = None;
        for (i, candidate) in candidates.iter().enumerate() {
            if rejected.contains_key(&i) {
                continue;
            }
            for req in candidate.requires.iter().filter(|r| !r.optional) {
                if by_name
                    .get(req.name.as_str())
                    .is_some_and(|j| rejected.contains_key(j))
                {
                    added = Some((i, Rejection::DependencyRejected(req.name.clone())));
                    break;
                }
            }
            if added.is_some() {
                break;
            }
        }
        match added {
            Some((i, reason)) => {
                rejected.insert(i, reason);
            }
            None => return,
        }
    }
}

/// Kahn's algorithm over the accepted candidates; whatever is left over is a cycle.
fn toposort(
    candidates: &[Candidate],
    by_name: &BTreeMap<&str, usize>,
    rejected: &mut BTreeMap<usize, Rejection>,
) -> Vec<usize> {
    let pending: Vec<usize> = (0..candidates.len())
        .filter(|i| !rejected.contains_key(i))
        .collect();
    let needs = |i: usize| -> Vec<usize> {
        candidates[i]
            .requires
            .iter()
            .filter_map(|r| by_name.get(r.name.as_str()).copied())
            .filter(|j| !rejected.contains_key(j))
            .collect()
    };

    let mut order: Vec<usize> = Vec::with_capacity(pending.len());
    let mut remaining = pending;
    while !remaining.is_empty() {
        let ready: Vec<usize> = remaining
            .iter()
            .copied()
            .filter(|&i| needs(i).iter().all(|j| order.contains(j)))
            .collect();
        if ready.is_empty() {
            break;
        }
        order.extend(&ready);
        remaining.retain(|i| !ready.contains(i));
    }
    if !remaining.is_empty() {
        let names: Vec<&str> = remaining
            .iter()
            .map(|&i| candidates[i].name.as_str())
            .collect();
        let joined = names.join(" -> ");
        for i in remaining {
            rejected.insert(i, Rejection::Cycle(joined.clone()));
        }
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(text: &str) -> VersionReq {
        VersionReq::parse(text).unwrap_or_else(|e| panic!("{e}"))
    }

    fn candidate(name: &str, version: &str, requires: &[(&str, &str, bool)]) -> Candidate {
        Candidate {
            name: name.to_owned(),
            version: version.to_owned(),
            requires: requires
                .iter()
                .map(|&(n, v, optional)| PluginReq {
                    name: n.to_owned(),
                    version: req(v),
                    optional,
                })
                .collect(),
        }
    }

    #[test]
    fn versions_compare_componentwise() {
        assert!(req("").is_any());
        assert!(req("").matches("anything"));
        assert!(req("1.2").matches("1.2.0"), "bare means at least");
        assert!(req("1.2").matches("1.10"));
        assert!(!req("1.2").matches("1.1.9"));
        assert!(req(">=1.9").matches("1.10"), "numeric, not lexicographic");
        assert!(req("=1.2.0").matches("1.2"), "missing component is zero");
        assert!(!req("=1.2.1").matches("1.2"));
        assert!(req(">=1.2, <2").matches("1.9.9"));
        assert!(!req(">=1.2, <2").matches("2.0"));
        assert!(req("<=1.2").matches("1.2"));
        assert!(!req(">1.2").matches("1.2"));
        assert!(req("=1.0-beta").matches("1.0-BETA"), "case insensitive");
        assert_eq!(
            VersionReq::parse(">="),
            Err(ReqError::MissingVersion(">=".into()))
        );
        assert_eq!(req(">= 1.2 ").as_str(), ">= 1.2");
    }

    #[test]
    fn dependencies_start_first() {
        let plan = plan(&[
            candidate("ui", "1.0", &[("vr", ">=1.0", false)]),
            candidate("vr", "1.4", &[("data", "", false)]),
            candidate("data", "2.0", &[]),
        ]);
        assert_eq!(plan.rejected, vec![]);
        let names = |p: &Plan| p.order.clone();
        assert_eq!(names(&plan), vec![2, 1, 0], "data, vr, ui");
    }

    #[test]
    fn missing_and_mismatched_requirements_are_rejected() {
        let plan = plan(&[
            candidate("a", "1.0", &[("nope", "", false)]),
            candidate("b", "1.0", &[("c", ">=2", false)]),
            candidate("c", "1.0", &[]),
            candidate("d", "1.0", &[("nope", "", true)]),
        ]);
        assert_eq!(plan.order, vec![2, 3], "c and d still start");
        assert_eq!(
            plan.rejected,
            vec![
                (0, Rejection::Missing("nope".into())),
                (
                    1,
                    Rejection::Version {
                        name: "c".into(),
                        req: ">=2".into(),
                        version: "1.0".into()
                    }
                ),
            ]
        );
    }

    #[test]
    fn rejection_propagates_but_stops_at_optional() {
        let plan = plan(&[
            candidate("a", "1.0", &[("missing", "", false)]),
            candidate("b", "1.0", &[("a", "", false)]),
            candidate("c", "1.0", &[("b", "", false)]),
            candidate("d", "1.0", &[("a", "", true)]),
        ]);
        assert_eq!(plan.order, vec![3]);
        assert_eq!(plan.rejected.len(), 3);
        assert_eq!(
            plan.rejected[2],
            (2, Rejection::DependencyRejected("b".into()))
        );
    }

    #[test]
    fn cycles_reject_everyone_in_them() {
        let plan = plan(&[
            candidate("a", "1.0", &[("b", "", false)]),
            candidate("b", "1.0", &[("a", "", false)]),
            candidate("c", "1.0", &[]),
        ]);
        assert_eq!(plan.order, vec![2]);
        assert_eq!(
            plan.rejected,
            vec![
                (0, Rejection::Cycle("a -> b".into())),
                (1, Rejection::Cycle("a -> b".into())),
            ]
        );
    }

    #[test]
    fn duplicate_names_keep_the_first() {
        let plan = plan(&[
            candidate("a", "1.0", &[]),
            candidate("a", "2.0", &[]),
            candidate("b", "1.0", &[("a", "=1.0", false)]),
        ]);
        assert_eq!(plan.order, vec![0, 2]);
        assert_eq!(
            plan.rejected,
            vec![(1, Rejection::DuplicateName("a".into()))]
        );
    }

    #[test]
    fn an_optional_dependency_still_orders() {
        let plan = plan(&[
            candidate("late", "1.0", &[("early", "", true)]),
            candidate("early", "1.0", &[]),
        ]);
        assert_eq!(plan.order, vec![1, 0]);
    }
}
