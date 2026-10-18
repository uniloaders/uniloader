//! Where the overlay's windows were left, and the editor preferences beside them, kept in
//! `<config>/overlay-windows.toml`.
//!
//! Any window that comes back — the console, the settings editor, a plugin's panel — has an
//! id, and that id is what this remembers it by: position, size and opacity. Toasts and
//! dialogs are not in here, because there is nothing to come back to.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use uniloader_core::store;
use uniloader_sdk::Host;

/// Least opacity a window can be set to; lower and it could be lost on screen.
pub(crate) const MIN_ALPHA: f32 = 0.2;

/// Prefix of the per-window keys in the store.
const WINDOW_PREFIX: &str = "window.";
/// Key of the advanced-settings preference.
const ADVANCED_KEY: &str = "ui.advanced";
/// File name under the config directory.
const FILE: &str = "overlay-windows.toml";

/// Where one window was and how transparent it was, in points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Geometry {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
    /// `1.0` is opaque, [`MIN_ALPHA`] the most transparent allowed.
    pub alpha: f32,
}

impl Geometry {
    /// `"<x> <y> <w> <h> <alpha>"`, the stored form.
    fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split_whitespace();
        let mut next = || parts.next()?.parse::<f32>().ok().filter(|n| n.is_finite());
        let geometry = Geometry {
            x: next()?,
            y: next()?,
            w: next()?,
            h: next()?,
            alpha: next().unwrap_or(1.0),
        };
        if geometry.w < 1.0 || geometry.h < 1.0 {
            return None;
        }
        Some(geometry.clamped())
    }

    /// The same geometry with the opacity brought into range.
    #[must_use]
    pub(crate) fn clamped(mut self) -> Self {
        self.alpha = if self.alpha.is_finite() {
            self.alpha.clamp(MIN_ALPHA, 1.0)
        } else {
            1.0
        };
        self
    }

    fn encode(&self) -> String {
        format!(
            "{:.0} {:.0} {:.0} {:.0} {:.2}",
            self.x, self.y, self.w, self.h, self.alpha
        )
    }

    /// Whether the two differ by enough to be worth writing to disk.
    fn differs_from(&self, other: &Self) -> bool {
        (self.x - other.x).abs() > 0.5
            || (self.y - other.y).abs() > 0.5
            || (self.w - other.w).abs() > 0.5
            || (self.h - other.h).abs() > 0.5
            || (self.alpha - other.alpha).abs() > 0.01
    }
}

/// Remembered geometry per window id, plus the editor's own preference.
#[derive(Debug, Clone, Default)]
pub(crate) struct Layout {
    windows: BTreeMap<String, Geometry>,
    advanced: bool,
    dirty: bool,
}

impl Layout {
    /// Read a store; unparseable entries are dropped.
    #[must_use]
    pub(crate) fn from_store(values: &BTreeMap<String, String>) -> Self {
        let mut layout = Layout::default();
        for (key, value) in values {
            if let Some(id) = key.strip_prefix(WINDOW_PREFIX) {
                if let Some(geometry) = Geometry::parse(value) {
                    layout.windows.insert(id.to_owned(), geometry);
                }
            } else if key == ADVANCED_KEY {
                layout.advanced = matches!(value.trim(), "true" | "1" | "yes" | "on");
            }
        }
        layout
    }

    /// The store to write back.
    #[must_use]
    pub(crate) fn to_store(&self) -> BTreeMap<String, String> {
        let mut values: BTreeMap<String, String> = self
            .windows
            .iter()
            .map(|(id, geometry)| (format!("{WINDOW_PREFIX}{id}"), geometry.encode()))
            .collect();
        values.insert(ADVANCED_KEY.to_owned(), self.advanced.to_string());
        values
    }

    /// Where this window was last seen.
    #[must_use]
    pub(crate) fn get(&self, id: &str) -> Option<Geometry> {
        self.windows.get(id).copied()
    }

    /// Note where a window is now; only a real change marks the layout for saving.
    pub(crate) fn remember(&mut self, id: &str, geometry: Geometry) {
        let geometry = geometry.clamped();
        match self.windows.get(id) {
            Some(previous) if !geometry.differs_from(previous) => {}
            _ => {
                self.windows.insert(id.to_owned(), geometry);
                self.dirty = true;
            }
        }
    }

    /// Whether the settings editor shows advanced settings.
    #[must_use]
    pub(crate) fn advanced(&self) -> bool {
        self.advanced
    }

    /// Show or hide advanced settings.
    pub(crate) fn set_advanced(&mut self, advanced: bool) {
        if advanced != self.advanced {
            self.advanced = advanced;
            self.dirty = true;
        }
    }

    /// Whether anything changed since the last call, clearing the flag.
    pub(crate) fn take_dirty(&mut self) -> bool {
        core::mem::take(&mut self.dirty)
    }
}

/// The layout and the file it lives in.
struct Stored {
    layout: Layout,
    path: Option<PathBuf>,
}

static STORED: Mutex<Stored> = Mutex::new(Stored {
    layout: Layout {
        windows: BTreeMap::new(),
        advanced: false,
        dirty: false,
    },
    path: None,
});

fn stored() -> MutexGuard<'static, Stored> {
    STORED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Lock the layout for reading or changing it.
pub(crate) fn with<R>(f: impl FnOnce(&mut Layout) -> R) -> R {
    f(&mut stored().layout)
}

/// Read the file, once, at start. A missing file is an empty layout.
pub(crate) fn load(host: &Host) {
    let path = host.config_dir().join(FILE);
    let values = if path.exists() {
        store::read(&path).unwrap_or_else(|e| {
            log::warn!("ignoring {}: {e}", path.display());
            BTreeMap::new()
        })
    } else {
        BTreeMap::new()
    };
    let mut guard = stored();
    guard.layout = Layout::from_store(&values);
    guard.path = Some(path);
}

/// Write the file if anything changed since the last write.
pub(crate) fn save() {
    let mut guard = stored();
    if !guard.layout.take_dirty() {
        return;
    }
    let Some(path) = guard.path.clone() else {
        return;
    };
    let values = guard.layout.to_store();
    drop(guard);
    if let Err(e) = store::write(&path, &values) {
        log::warn!("could not save the window layout: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_of(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn geometry(x: f32, alpha: f32) -> Geometry {
        Geometry {
            x,
            y: 10.0,
            w: 200.0,
            h: 100.0,
            alpha,
        }
    }

    #[test]
    fn a_layout_survives_a_round_trip() {
        let layout = Layout::from_store(&store_of(&[
            ("window.overlay.console", "120 80 640 360 0.90"),
            ("ui.advanced", "true"),
        ]));
        assert!(layout.advanced());
        let console = layout.get("overlay.console");
        assert_eq!(console.map(|g| (g.x, g.w)), Some((120.0, 640.0)));
        let again = Layout::from_store(&layout.to_store());
        assert_eq!(again.get("overlay.console"), console);
        assert!(again.advanced());
    }

    #[test]
    fn nonsense_entries_are_dropped() {
        let layout = Layout::from_store(&store_of(&[
            ("window.a", "nope"),
            ("window.b", "1 2 0 0 1"),
            ("window.c", "1 2 300 200"),
        ]));
        assert_eq!(layout.get("a"), None);
        assert_eq!(layout.get("b"), None, "a zero-sized window is not restored");
        assert_eq!(layout.get("c").map(|g| g.alpha), Some(1.0));
    }

    #[test]
    fn only_a_real_change_asks_for_a_save() {
        let mut layout = Layout::default();
        layout.remember("panel", geometry(10.0, 1.0));
        assert!(layout.take_dirty(), "a window seen for the first time");
        layout.remember("panel", geometry(10.0, 1.0));
        assert!(!layout.take_dirty(), "the same place again");
        layout.remember("panel", geometry(10.2, 1.0));
        assert!(!layout.take_dirty(), "a fifth of a point is not a move");
        layout.remember("panel", geometry(40.0, 1.0));
        assert!(layout.take_dirty());
    }

    #[test]
    fn opacity_cannot_be_set_to_invisible() {
        let mut layout = Layout::default();
        layout.remember("panel", geometry(0.0, 0.0));
        assert_eq!(layout.get("panel").map(|g| g.alpha), Some(MIN_ALPHA));
    }
}
