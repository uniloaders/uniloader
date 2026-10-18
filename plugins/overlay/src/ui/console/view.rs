//! The scrollback half of the console: one row per line, coloured by where it came from.
//!
//! Each row is one galley rather than a row of labels: hundreds of lines at four widgets each
//! would be thousands of widgets a frame, and one galley is also what makes wrapping and
//! selection work across the whole row.

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Label, ScrollArea, TextStyle, Ui};
use uniloader_sdk::{ConsoleEntry, ConsoleKind, LogLevel};

/// Width the level column is padded to, so every row's text starts at the same place.
const TAG_WIDTH: usize = 5;

/// Which columns the console shows.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Columns {
    /// The `HH:MM:SS` stamp.
    pub time: bool,
    /// The module a log record came from.
    pub target: bool,
    /// Whether long lines wrap.
    pub wrap: bool,
}

/// Short tag for the level column.
fn tag(line: &ConsoleEntry) -> &'static str {
    match (line.kind, line.level) {
        (ConsoleKind::ECHO, _) => "cmd",
        (ConsoleKind::LOG, LogLevel::ERROR) => "ERROR",
        (ConsoleKind::LOG, LogLevel::WARN) => "WARN",
        (ConsoleKind::LOG, LogLevel::INFO) => "INFO",
        (ConsoleKind::LOG, LogLevel::DEBUG) => "DEBUG",
        (ConsoleKind::LOG, _) => "TRACE",
        _ => "out",
    }
}

/// The colour a line is drawn in.
fn colour(line: &ConsoleEntry, ui: &Ui) -> Color32 {
    match (line.kind, line.level) {
        (ConsoleKind::ECHO, _) => Color32::from_rgb(150, 205, 255),
        (ConsoleKind::LOG, LogLevel::ERROR) => Color32::from_rgb(240, 120, 120),
        (ConsoleKind::LOG, LogLevel::WARN) => Color32::from_rgb(235, 195, 105),
        (ConsoleKind::LOG, LogLevel::INFO) => Color32::from_rgb(175, 195, 215),
        (ConsoleKind::LOG, LogLevel::DEBUG) => Color32::from_gray(140),
        (ConsoleKind::LOG, _) => Color32::from_gray(115),
        _ => ui.visuals().text_color(),
    }
}

/// Lay one line out as a single galley.
fn row(line: &ConsoleEntry, ui: &Ui, columns: Columns, width: f32) -> LayoutJob {
    let font: FontId = TextStyle::Monospace.resolve(ui.style());
    let dim = Color32::from_gray(110);
    let body = colour(line, ui);
    let mut job = LayoutJob::default();
    let mut put = |text: &str, colour: Color32| {
        job.append(
            text,
            0.0,
            TextFormat {
                font_id: font.clone(),
                color: colour,
                ..TextFormat::default()
            },
        );
    };
    if columns.time {
        put(&format!("{} ", line.clock), dim);
    }
    if line.kind == ConsoleKind::LOG {
        put(&format!("{:<TAG_WIDTH$} ", tag(line)), body);
        if columns.target && !line.target.is_empty() {
            put(&format!("{} ", short_target(&line.target)), dim);
        }
    }
    put(&line.text, body);
    job.wrap.max_width = if columns.wrap { width } else { f32::INFINITY };
    job
}

/// A log target without its crate prefix, which is the same on most lines and costs width.
fn short_target(target: &str) -> &str {
    target.split_once("::").map_or(target, |(_, rest)| rest)
}

/// Draw the scrollback. Returns how many lines the filter hid.
pub(crate) fn show(
    ui: &mut Ui,
    lines: &[ConsoleEntry],
    filter: &Filter,
    columns: Columns,
) -> usize {
    let shown: Vec<&ConsoleEntry> = lines.iter().filter(|line| filter.passes(line)).collect();
    let hidden = lines.len() - shown.len();
    let width = ui.available_width();
    ScrollArea::both()
        .stick_to_bottom(filter.follow)
        .auto_shrink([false, false])
        .show_rows(
            ui,
            ui.text_style_height(&TextStyle::Monospace),
            shown.len(),
            |ui, range| {
                // Only the rows in view are laid out.
                for line in shown.get(range).unwrap_or_default() {
                    ui.add(Label::new(row(line, ui, columns, width)).selectable(true));
                }
            },
        );
    hidden
}

/// What the console is showing of its buffer.
#[derive(Debug, Clone)]
pub(crate) struct Filter {
    /// Lowest log level shown. Command output is never filtered out.
    pub level: log::LevelFilter,
    /// Substring a line must contain, case insensitively.
    pub text: String,
    /// Whether the view sticks to the newest line.
    pub follow: bool,
    /// Lines up to and including this sequence number are hidden. Clearing hides rather than
    /// deletes: the buffer is the loader's, and other readers still want it.
    pub cleared_at: u64,
}

impl Default for Filter {
    fn default() -> Self {
        Filter {
            level: log::LevelFilter::Info,
            text: String::new(),
            follow: true,
            cleared_at: 0,
        }
    }
}

impl Filter {
    /// Whether a line is shown.
    pub(crate) fn passes(&self, line: &ConsoleEntry) -> bool {
        if line.seq <= self.cleared_at {
            return false;
        }
        // Log levels count up from error = 1, as in the `log` crate.
        if line.kind == ConsoleKind::LOG && line.level.0 as usize > self.level as usize {
            return false;
        }
        if self.text.is_empty() {
            return true;
        }
        let needle = self.text.to_ascii_lowercase();
        line.text.to_ascii_lowercase().contains(&needle)
            || line.target.to_ascii_lowercase().contains(&needle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(kind: ConsoleKind, level: LogLevel, target: &str, text: &str) -> ConsoleEntry {
        ConsoleEntry {
            seq: 1,
            kind,
            level,
            clock: "12:00:00".to_owned(),
            target: target.to_owned(),
            text: text.to_owned(),
        }
    }

    #[test]
    fn the_crate_prefix_is_dropped_from_a_target() {
        assert_eq!(short_target("uniloader::win"), "win");
        assert_eq!(short_target("overlay"), "overlay");
    }

    #[test]
    fn the_filter_matches_text_or_target_and_respects_the_level() {
        let mut filter = Filter::default();
        let warning = line(ConsoleKind::LOG, LogLevel::WARN, "x::win", "no swapchain");
        let debug = line(ConsoleKind::LOG, LogLevel::DEBUG, "x::win", "hooked");
        assert!(filter.passes(&warning));
        assert!(!filter.passes(&debug), "debug is below the default level");
        filter.text = "SWAP".to_owned();
        assert!(filter.passes(&warning), "case insensitive");
        filter.text = "win".to_owned();
        assert!(filter.passes(&warning), "the target counts");
        filter.text.clear();
        filter.level = log::LevelFilter::Error;
        assert!(!filter.passes(&warning));
        let output = line(ConsoleKind::OUTPUT, LogLevel::INFO, "", "x = 1");
        assert!(filter.passes(&output), "command output is never filtered");
    }

    #[test]
    fn clearing_hides_up_to_the_mark() {
        let mut filter = Filter::default();
        let mut old = line(ConsoleKind::OUTPUT, LogLevel::INFO, "", "before");
        old.seq = 7;
        let mut new = old.clone();
        new.seq = 8;
        filter.cleared_at = 7;
        assert!(!filter.passes(&old));
        assert!(filter.passes(&new));
    }
}
