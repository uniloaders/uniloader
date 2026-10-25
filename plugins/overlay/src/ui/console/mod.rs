//! The loader's console, drawn in the game.
//!
//! It shows the loader's log as well as what commands print — what makes it worth opening
//! when nothing has been typed — read through `console_read`. The window is three panels
//! (toolbar, scrollback, input) so the input row stays on the bottom edge at every size.

mod complete;
mod view;

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use egui::{Context, Key, Modifiers, RichText, TextEdit};
use uniloader_sdk::{ConsoleEntry, Host};
use view::{Columns, Filter};

use super::chrome::{self, Chrome, Placed};

/// How many submitted lines the arrow keys can walk back through.
const HISTORY: usize = 100;
/// How many scrollback lines the window keeps.
const SCROLLBACK: usize = 2000;
/// Room kept right of the input box for the Run button.
const RUN_BUTTON_WIDTH: f32 = 44.0;
/// How long after a submitted line its output may still bring new names.
const NAMES_AFTER: Duration = Duration::from_secs(5);

/// What the input box's keys asked for this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pressed {
    Nothing,
    /// Enter: run the line.
    Submit,
    /// Escape on an empty line: close the console.
    Close,
}

/// What the scrollback holds, for the status line and the clear button.
#[derive(Debug, Clone, Copy)]
struct Stats {
    total: usize,
    hidden: usize,
    newest: u64,
}

/// What one frame of the console decided.
pub(crate) struct Outcome {
    /// A line the user submitted, to run after the frame.
    pub(crate) submitted: Option<String>,
    /// Set when the console asked to be closed.
    pub(crate) close: bool,
    /// Where the window ended up.
    pub(crate) placed: Placed,
}

/// What the console remembers between frames.
pub(crate) struct ConsolePanel {
    input: String,
    history: Vec<String>,
    recall: Option<usize>,
    /// Set when the input box should take the keyboard on the next frame.
    focus: bool,
    filter: Filter,
    columns: Columns,
    /// Candidates from the last ambiguous completion, shown under the input.
    offered: Vec<String>,
    /// The scrollback, newest last.
    lines: VecDeque<ConsoleEntry>,
    /// Where `console_read` continues from.
    mark: u64,
    /// What Tab completes against, refreshed when the console opens and after each command.
    names: Vec<String>,
    /// A submitted line runs on the console thread: until this moment the names are read
    /// again whenever something was printed (its echo comes first, its effects later).
    names_due: Option<Instant>,
}

impl Default for ConsolePanel {
    fn default() -> Self {
        ConsolePanel {
            input: String::new(),
            history: Vec::new(),
            recall: None,
            focus: false,
            filter: Filter::default(),
            columns: Columns {
                time: true,
                target: true,
                wrap: true,
            },
            offered: Vec::new(),
            lines: VecDeque::new(),
            mark: 0,
            names: Vec::new(),
            names_due: None,
        }
    }
}

impl ConsolePanel {
    /// Called when the window opens: take the keyboard and learn the names.
    pub(crate) fn opened(&mut self, host: &Host) {
        self.focus = true;
        self.names = host.console_names();
    }

    /// Called when a line was submitted: it may load a plugin with new names (or define an
    /// alias), which [`ConsolePanel::pull`] picks up once the line printed.
    pub(crate) fn ran(&mut self) {
        self.names_due = Some(Instant::now() + NAMES_AFTER);
    }

    /// Pull whatever was printed since the last frame.
    pub(crate) fn pull(&mut self, host: &Host) {
        let (fresh, next) = host.console_read(self.mark);
        self.mark = next;
        self.names_due = self.names_due.filter(|until| Instant::now() < *until);
        if self.names_due.is_some() && !fresh.is_empty() {
            self.names = host.console_names();
        }
        self.lines.extend(fresh);
        let excess = self.lines.len().saturating_sub(SCROLLBACK);
        self.lines.drain(..excess);
    }

    /// Draw the window.
    pub(crate) fn show(&mut self, ctx: &Context, chrome: &Chrome<'_>) -> Outcome {
        // Moved out for the frame rather than cloned: the closures below need `self` too.
        let mut buffer = core::mem::take(&mut self.lines);
        let outcome = self.show_lines(ctx, chrome, buffer.make_contiguous());
        self.lines = buffer;
        outcome
    }

    fn show_lines(
        &mut self,
        ctx: &Context,
        chrome: &Chrome<'_>,
        lines: &[ConsoleEntry],
    ) -> Outcome {
        let mut outcome = Outcome {
            submitted: None,
            close: false,
            placed: Placed {
                geometry: None,
                open: true,
            },
        };
        let mut stats = Stats {
            total: lines.len(),
            hidden: 0,
            newest: lines.last().map_or(0, |line| line.seq),
        };
        outcome.placed = chrome::show(ctx, chrome, |ui| {
            egui::Panel::top(ui.id().with("toolbar"))
                .show(ui, |ui| self.toolbar(ui, lines, stats.newest));
            egui::Panel::bottom(ui.id().with("input")).show(ui, |ui| {
                self.prompt(ui, &mut outcome, stats);
            });
            egui::CentralPanel::no_frame().show(ui, |ui| {
                stats.hidden = view::show(ui, lines, &self.filter, self.columns);
            });
        });
        outcome
    }

    /// Clear, copy, the level filter, the search box and the view toggles.
    fn toolbar(&mut self, ui: &mut egui::Ui, lines: &[ConsoleEntry], newest: u64) {
        ui.horizontal_wrapped(|ui| {
            if ui
                .button("Clear")
                .on_hover_text("Hide everything above this point (ctrl+L)")
                .clicked()
            {
                self.filter.cleared_at = newest;
            }
            if ui
                .button("Copy")
                .on_hover_text("Copy what is shown to the clipboard")
                .clicked()
            {
                ui.ctx().copy_text(self.copy(lines));
            }
            ui.separator();
            egui::ComboBox::from_id_salt(ui.id().with("level"))
                .selected_text(level_name(self.filter.level))
                .width(78.0)
                .show_ui(ui, |ui| {
                    for level in [
                        log::LevelFilter::Error,
                        log::LevelFilter::Warn,
                        log::LevelFilter::Info,
                        log::LevelFilter::Debug,
                        log::LevelFilter::Trace,
                    ] {
                        ui.selectable_value(&mut self.filter.level, level, level_name(level));
                    }
                })
                .response
                .on_hover_text("Lowest log level shown. Command output is always shown.");
            ui.separator();
            ui.label("Find");
            ui.add(
                TextEdit::singleline(&mut self.filter.text)
                    .desired_width(120.0)
                    .hint_text("text"),
            );
            if !self.filter.text.is_empty() && super::icons::close(ui, "Clear the search") {
                self.filter.text.clear();
            }
            ui.separator();
            ui.checkbox(&mut self.filter.follow, "Follow")
                .on_hover_text("Stay at the newest line");
            ui.menu_button("View", |ui| {
                ui.checkbox(&mut self.columns.wrap, "Wrap long lines");
                ui.checkbox(&mut self.columns.time, "Show the time");
                ui.checkbox(&mut self.columns.target, "Show the module");
            });
        });
    }

    /// The prompt, the completion hint and the status line.
    fn prompt(&mut self, ui: &mut egui::Ui, outcome: &mut Outcome, stats: Stats) {
        if !self.offered.is_empty() {
            ui.label(
                RichText::new(self.offered.join("  "))
                    .monospace()
                    .color(egui::Color32::from_gray(150)),
            );
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new(">").monospace());
            // Keys are taken out of the queue before the box is drawn, while last frame's
            // focus still says whether they were meant for it: egui's single-line edit
            // answers Enter by giving up focus, after which the line would be lost.
            let id = ui.id().with("line");
            let focused = ui.memory(|memory| memory.has_focus(id));
            let pressed = focused.then(|| self.take_keys(ui, stats.newest));
            let response = ui.add(
                TextEdit::singleline(&mut self.input)
                    .id(id)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("type `help`")
                    .desired_width((ui.available_width() - RUN_BUTTON_WIDTH).max(80.0)),
            );
            let run = ui.add_enabled(!self.input.trim().is_empty(), egui::Button::new("Run"));
            if self.focus {
                response.request_focus();
                self.focus = false;
            }
            let pressed = if run.clicked() {
                Some(Pressed::Submit)
            } else {
                pressed
            };
            match pressed {
                Some(Pressed::Submit) => {
                    outcome.submitted = self.submit();
                    self.offered.clear();
                    response.request_focus();
                }
                Some(Pressed::Close) => outcome.close = true,
                Some(Pressed::Nothing) | None => {}
            }
        });
        let hidden_note = if stats.hidden == 0 {
            String::new()
        } else {
            format!(", {} hidden by the filter", stats.hidden)
        };
        ui.label(
            RichText::new(format!(
                "{} lines{hidden_note} — Tab completes, Up recalls, Enter or Run runs",
                stats.total
            ))
            .small()
            .color(egui::Color32::from_gray(130)),
        );
    }

    /// Take the keys the input box answers to out of this frame's queue.
    fn take_keys(&mut self, ui: &mut egui::Ui, newest: u64) -> Pressed {
        // Tab would otherwise move the focus away, closing the keyboard on completion.
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Tab)) {
            self.complete();
        }
        if ui.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::L)) {
            self.filter.cleared_at = newest;
        }
        self.recall_history(ui);
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
            return Pressed::Submit;
        }
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            // The first Escape drops a half-typed line, the second closes the window.
            if self.input.is_empty() {
                return Pressed::Close;
            }
            self.input.clear();
            self.offered.clear();
        }
        Pressed::Nothing
    }

    /// Complete the typed line, or offer what it could be.
    fn complete(&mut self) {
        self.offered.clear();
        let Some(completion) = complete::complete(&self.input, &self.names) else {
            return;
        };
        self.input = completion.line;
        if completion.matches.len() > 1 {
            self.offered = completion.matches;
            self.offered.truncate(12);
        }
        // So the caret follows the completed text.
        self.focus = true;
    }

    /// What the console is showing, as text for the clipboard.
    fn copy(&self, lines: &[ConsoleEntry]) -> String {
        use std::fmt::Write as _;
        let mut text = String::new();
        for line in lines.iter().filter(|line| self.filter.passes(line)) {
            let _ = writeln!(text, "{} {}", line.clock, line.text);
        }
        text
    }

    /// Walk the submitted-line history with the arrow keys.
    fn recall_history(&mut self, ui: &egui::Ui) {
        let (up, down) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
            )
        });
        if self.history.is_empty() || !(up || down) {
            return;
        }
        let last = self.history.len() - 1;
        self.recall = match (self.recall, up) {
            (None, true) => Some(last),
            (Some(0), true) => Some(0),
            (Some(n), true) => Some(n - 1),
            (Some(n), false) if n >= last => None,
            (Some(n), false) => Some(n + 1),
            (None, false) => None,
        };
        self.input = self
            .recall
            .and_then(|n| self.history.get(n))
            .cloned()
            .unwrap_or_default();
        self.focus = true;
    }

    /// Take the typed line, remember it, and hand it back for running.
    fn submit(&mut self) -> Option<String> {
        let line = self.input.trim().to_owned();
        self.input.clear();
        self.recall = None;
        if line.is_empty() {
            return None;
        }
        if self.history.last() != Some(&line) {
            self.history.push(line.clone());
        }
        if self.history.len() > HISTORY {
            self.history.remove(0);
        }
        Some(line)
    }
}

/// The name a level filter goes by in the toolbar.
fn level_name(level: log::LevelFilter) -> &'static str {
    match level {
        log::LevelFilter::Off => "Nothing",
        log::LevelFilter::Error => "Errors",
        log::LevelFilter::Warn => "Warnings",
        log::LevelFilter::Info => "Info",
        log::LevelFilter::Debug => "Debug",
        log::LevelFilter::Trace => "Trace",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_is_bounded_and_skips_repeats() {
        let mut panel = ConsolePanel::default();
        for i in 0..HISTORY + 10 {
            panel.input = format!("line {i}");
            assert!(panel.submit().is_some());
        }
        assert_eq!(panel.history.len(), HISTORY);
        assert_eq!(panel.history.first().map(String::as_str), Some("line 10"));
        panel.input = format!("line {}", HISTORY + 9);
        assert!(panel.submit().is_some());
        assert_eq!(panel.history.len(), HISTORY, "a repeat is not stored twice");
        panel.input = "   ".to_owned();
        assert_eq!(panel.submit(), None);
    }
}
