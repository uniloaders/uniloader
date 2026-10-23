//! What the editor keeps between frames besides the files: text being typed, the message
//! under each field, and which binding is recording a key.

use std::collections::HashMap;

/// What a key recording is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    /// A key setting, `<plugin>.<key>`.
    Setting(String),
    /// A plugin's suggested bind entry, by qualified name.
    Entry(String),
    /// The key of the bind being added or edited.
    Bind,
}

/// Field state between frames.
#[derive(Debug, Default)]
pub struct Edit {
    /// Text being typed, by field id.
    drafts: HashMap<String, String>,
    /// Why the text of a field was not taken, by field id.
    errors: HashMap<String, String>,
    /// Remarks that did not stop a change (unknown commands), by field id.
    notes: HashMap<String, Vec<String>>,
    /// The binding recording a key, if any.
    pub recording: Option<Record>,
    /// Show settings marked advanced.
    pub show_advanced: bool,
}

impl Edit {
    /// The draft of field `id`, started from `initial` when there is none.
    pub fn draft(&mut self, id: &str, initial: impl FnOnce() -> String) -> &mut String {
        self.drafts.entry(id.to_owned()).or_insert_with(initial)
    }

    /// Forget the draft and messages of field `id`, so it shows the stored value again.
    pub fn forget(&mut self, id: &str) {
        self.drafts.remove(id);
        self.errors.remove(id);
        self.notes.remove(id);
    }

    /// Forget every draft, for example after reloading the files.
    pub fn forget_all(&mut self) {
        self.drafts.clear();
        self.errors.clear();
        self.notes.clear();
        self.recording = None;
    }

    /// Record the outcome of taking a field's text: an error, or notes (possibly none).
    pub fn outcome(&mut self, id: &str, outcome: Result<Vec<String>, String>) {
        match outcome {
            Ok(notes) => {
                self.errors.remove(id);
                self.notes.insert(id.to_owned(), notes);
            }
            Err(error) => {
                self.notes.remove(id);
                self.errors.insert(id.to_owned(), error);
            }
        }
    }

    /// Why field `id`'s text was not taken.
    #[must_use]
    pub fn error(&self, id: &str) -> Option<&str> {
        self.errors.get(id).map(String::as_str)
    }

    /// Remarks about field `id`'s last change.
    #[must_use]
    pub fn notes(&self, id: &str) -> &[String] {
        self.notes.get(id).map_or(&[], Vec::as_slice)
    }

    /// Whether `target` is recording.
    #[must_use]
    pub fn is_recording(&self, target: &Record) -> bool {
        self.recording.as_ref() == Some(target)
    }

    /// Start recording for `target`, or stop when it already is.
    pub fn toggle_recording(&mut self, target: Record) {
        self.recording = if self.is_recording(&target) {
            None
        } else {
            Some(target)
        };
    }
}

/// Show the error and notes of field `id` under it.
pub fn messages(ui: &mut eframe::egui::Ui, edit: &Edit, id: &str) {
    if let Some(error) = edit.error(id) {
        ui.colored_label(ui.visuals().error_fg_color, error);
    }
    for note in edit.notes(id) {
        ui.colored_label(ui.visuals().warn_fg_color, note);
    }
}
