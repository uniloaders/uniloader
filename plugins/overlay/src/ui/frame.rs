//! The widget half of the service: a consumer fills a panel body by calling `widget` with
//! the token its draw callback received.
//!
//! The token is a number, not a pointer: the `egui::Ui` it stands for lives on this thread's
//! stack for exactly the length of that callback, and a consumer that keeps the token and
//! calls later is told [`Status::WRONG_PHASE`] instead of having a stale pointer dereferenced.
//!
//! Setting writes are queued rather than made on the spot. Writing a setting notifies its
//! owner, and the owner is very likely the plugin whose draw callback is on the stack right
//! now; calling back into it from inside its own callback is how deadlocks happen.

use std::cell::{Cell, RefCell};

use uniloader_overlay::{UiValue, UiWidget};
use uniloader_sdk::api::{PluginHandle, Status};
use uniloader_sdk::{SettingEntry, SettingKind};

/// Widest integer range that still makes sense as a slider; above it, a number field.
const SLIDER_STEPS: f64 = 1000.0;
/// Most toggles one row can hold.
const MAX_TOGGLES: u32 = 32;
/// Label column width for a toggle row when the caller does not pick one.
const TOGGLE_LABEL_WIDTH: f32 = 220.0;

/// The panel body being filled on this thread.
#[derive(Clone, Copy)]
struct Current {
    token: u64,
    owner: PluginHandle,
    ui: *mut egui::Ui,
}

thread_local! {
    static CURRENT: Cell<Option<Current>> = const { Cell::new(None) };
    /// The owner's plugin name while its body is being filled, for unqualified setting keys.
    static OWNER_NAME: RefCell<String> = const { RefCell::new(String::new()) };
    /// Setting writes made during the frame: qualified name and new value.
    static PENDING: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
}

/// Run `body` with `ui` reachable through `token` for `owner`, then take it back.
pub(crate) fn with<R>(
    token: u64,
    owner: PluginHandle,
    owner_name: &str,
    ui: &mut egui::Ui,
    body: impl FnOnce() -> R,
) -> R {
    /// Puts the previous body back however `body` ends: a faulting or panicking consumer
    /// must not leave a pointer to a dead stack frame behind its token.
    struct Restore(Option<Current>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            CURRENT.with(|c| c.set(previous));
        }
    }
    let previous = CURRENT.with(|c| {
        c.replace(Some(Current {
            token,
            owner,
            ui: core::ptr::from_mut(ui),
        }))
    });
    let _restore = Restore(previous);
    OWNER_NAME.with_borrow_mut(|name| owner_name.clone_into(name));
    body()
}

/// A token no earlier body had, for the life of the process (a rebuilt renderer included).
pub(crate) fn next_token() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Whether `token` is the body being filled on this thread for `owner`: checked before any
/// argument of a `widget` call is read, as the ABI promises.
pub(crate) fn is_current(token: u64, owner: PluginHandle) -> bool {
    CURRENT.with(Cell::get).is_some_and(|current| {
        token != 0 && token == current.token && owner == current.owner && !current.ui.is_null()
    })
}

/// Take the setting writes queued during the frame.
pub(crate) fn take_pending() -> Vec<(String, String)> {
    PENDING.with_borrow_mut(core::mem::take)
}

/// Queue a setting write for after the frame.
pub(crate) fn queue_set(name: String, value: String) {
    PENDING.with_borrow_mut(|pending| pending.push((name, value)));
}

/// Borrow the panel body belonging to `token` and `owner`.
fn borrow<R>(token: u64, owner: PluginHandle, body: impl FnOnce(&mut egui::Ui) -> R) -> Option<R> {
    let current = CURRENT.with(Cell::get)?;
    if token == 0 || token != current.token || owner != current.owner || current.ui.is_null() {
        return None;
    }
    // SAFETY: the token matches the one `with` installed on this thread, so `ui` is the
    // `&mut egui::Ui` of the call still on this thread's stack. Nothing else holds a
    // reference to it while the consumer's callback runs, and this borrow ends first.
    #[allow(unsafe_code)]
    Some(body(unsafe { &mut *current.ui }))
}

/// Add one widget to a panel body. This is `OverlayApi::widget`.
pub(crate) fn widget(
    owner: PluginHandle,
    token: u64,
    kind: UiWidget,
    text: &str,
    value: Option<&mut UiValue>,
) -> Status {
    borrow(token, owner, |ui| draw(ui, kind, text, value)).unwrap_or(Status::WRONG_PHASE)
}

fn draw(ui: &mut egui::Ui, kind: UiWidget, text: &str, value: Option<&mut UiValue>) -> Status {
    match (kind, value) {
        (UiWidget::LABEL, _) => {
            ui.label(text);
        }
        (UiWidget::HEADING, _) => {
            ui.heading(text);
        }
        (UiWidget::SEPARATOR, _) => {
            ui.separator();
        }
        (UiWidget::SPACE, value) => {
            let points = value.map_or(0.0, |v| v.float);
            ui.add_space(if points > 0.0 { points } else { 6.0 });
        }
        (UiWidget::SETTING, _) => return setting(ui, text),
        (UiWidget::BUTTON, Some(out)) => out.boolean = ui.button(text).clicked(),
        (UiWidget::SELECTABLE, Some(out)) => {
            out.boolean = ui.selectable_label(out.boolean, text).clicked();
        }
        (UiWidget::GPU_TEXTURE, Some(out)) => {
            #[allow(
                clippy::cast_sign_loss,
                clippy::cast_possible_truncation,
                reason = "a handle carried bit for bit; sizes in points"
            )]
            let (handle, size) = (
                out.integer as usize,
                egui::vec2(out.min as f32, out.max as f32),
            );
            let fits = (1.0..=2000.0).contains(&size.x) && (1.0..=2000.0).contains(&size.y);
            if !fits {
                return Status::INVALID_ARGUMENT;
            }
            if !super::native::show(handle, size, text) {
                return Status::UNSUPPORTED;
            }
        }
        (UiWidget::CHECKBOX, Some(out)) => {
            ui.checkbox(&mut out.boolean, text);
        }
        (UiWidget::SLIDER, Some(out)) => {
            #[allow(clippy::cast_possible_truncation, reason = "slider bounds")]
            let (min, max) = (out.min as f32, out.max as f32);
            ui.add(egui::Slider::new(&mut out.float, min..=max).text(text));
        }
        (UiWidget::SLIDER_INT, Some(out)) => {
            #[allow(clippy::cast_possible_truncation, reason = "slider bounds")]
            let (min, max) = (out.min as i64, out.max as i64);
            ui.add(egui::Slider::new(&mut out.integer, min..=max).text(text));
        }
        (UiWidget::TOGGLES, Some(out)) => {
            out.integer = toggles(ui, text, out.integer, out.max, out.min);
        }
        (
            UiWidget::BUTTON
            | UiWidget::SELECTABLE
            | UiWidget::GPU_TEXTURE
            | UiWidget::CHECKBOX
            | UiWidget::SLIDER
            | UiWidget::SLIDER_INT
            | UiWidget::TOGGLES,
            None,
        ) => return Status::INVALID_ARGUMENT,
        // A consumer built against a newer table asking for a widget this overlay lacks.
        _ => return Status::UNSUPPORTED,
    }
    Status::OK
}

/// A labelled row of checkboxes over the bits of one integer, with a fixed label column so
/// the boxes line up down a panel.
fn toggles(ui: &mut egui::Ui, text: &str, bits: i64, count: f64, label_width: f64) -> i64 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped"
    )]
    let count = (count.clamp(1.0, f64::from(MAX_TOGGLES)) as u32).min(MAX_TOGGLES);
    #[allow(clippy::cast_possible_truncation, reason = "a width in points")]
    let width = if label_width > 0.0 {
        label_width as f32
    } else {
        TOGGLE_LABEL_WIDTH
    };
    let mut bits = bits;
    ui.horizontal(|ui| {
        let height = ui.spacing().interact_size.y;
        let layout = egui::Layout::left_to_right(egui::Align::Center);
        ui.allocate_ui_with_layout(egui::vec2(width, height), layout, |ui| {
            ui.add(egui::Label::new(text).truncate());
        });
        for index in 0..count {
            let mask = 1i64 << index;
            let mut on = bits & mask != 0;
            ui.checkbox(&mut on, "");
            if on {
                bits |= mask;
            } else {
                bits &= !mask;
            }
        }
    });
    bits
}

/// The control for a registered setting, written through the loader after the frame.
fn setting(ui: &mut egui::Ui, name: &str) -> Status {
    let Some(host) = crate::host() else {
        return Status::ERROR;
    };
    let qualified = if name.contains('.') {
        name.to_owned()
    } else {
        OWNER_NAME.with_borrow(|owner| format!("{owner}.{name}"))
    };
    let Some((plugin, key)) = qualified.split_once('.') else {
        return Status::INVALID_ARGUMENT;
    };
    let Some(entry) = host
        .settings(Some(plugin))
        .into_iter()
        .find(|s| s.key == key)
    else {
        return Status::NOT_FOUND;
    };
    if let Some(value) = row(ui, &entry, &label_of(&entry)) {
        queue_set(qualified, value);
    }
    if !entry.description.is_empty() {
        ui.label(egui::RichText::new(&entry.description).weak().small());
    }
    Status::OK
}

/// A setting's label: its title, or its key.
pub(crate) fn label_of(entry: &SettingEntry) -> String {
    if entry.title.is_empty() {
        entry.key.clone()
    } else {
        entry.title.clone()
    }
}

/// One setting as the user meets it: its control, and a reset once it is not the default.
pub(crate) fn row(ui: &mut egui::Ui, entry: &SettingEntry, label: &str) -> Option<String> {
    ui.push_id((&entry.plugin, &entry.key), |ui| {
        ui.horizontal(|ui| {
            let changed = control(ui, entry, label);
            if entry.value == entry.default {
                return changed;
            }
            let hint = if entry.default.is_empty() {
                "Reset to nothing".to_owned()
            } else {
                format!("Reset to {:?}", entry.default)
            };
            if super::icons::revert(ui, &hint) {
                return Some(entry.default.clone());
            }
            changed
        })
        .inner
    })
    .inner
}

/// The control for one setting kind; the new value when the user changed it.
fn control(ui: &mut egui::Ui, entry: &SettingEntry, label: &str) -> Option<String> {
    let current = entry.value.as_str();
    if entry.secret {
        return super::controls::secret(ui, entry, label);
    }
    match entry.kind {
        SettingKind::Bool => {
            let mut on = current == "true";
            ui.checkbox(&mut on, label)
                .changed()
                .then(|| on.to_string())
        }
        SettingKind::Int => {
            let mut v = current.parse::<i64>().unwrap_or_default();
            #[allow(
                clippy::cast_possible_truncation,
                reason = "bounds of an integer setting"
            )]
            let range = (entry.min as i64)..=(entry.max as i64);
            // Sixty thousand values across eighty pixels is not a choice a person can make,
            // so a wide range gets a field that still clamps.
            let response = if entry.min < entry.max && entry.max - entry.min <= SLIDER_STEPS {
                ui.add(egui::Slider::new(&mut v, range).text(label))
            } else {
                ui.horizontal(|ui| {
                    let mut drag = egui::DragValue::new(&mut v);
                    if entry.min < entry.max {
                        drag = drag.range(range);
                    }
                    let r = ui.add(drag);
                    ui.label(label);
                    r
                })
                .inner
            };
            response.changed().then(|| v.to_string())
        }
        SettingKind::Float => {
            let mut v = current.parse::<f64>().unwrap_or_default();
            let response = if entry.min < entry.max {
                ui.add(egui::Slider::new(&mut v, entry.min..=entry.max).text(label))
            } else {
                ui.horizontal(|ui| {
                    let r = ui.add(egui::DragValue::new(&mut v).speed(0.01));
                    ui.label(label);
                    r
                })
                .inner
            };
            response.changed().then(|| v.to_string())
        }
        SettingKind::Enum => {
            let mut picked = None;
            egui::ComboBox::from_label(label)
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for choice in &entry.choices {
                        if ui.selectable_label(choice == current, choice).clicked() {
                            picked = Some(choice.clone());
                        }
                    }
                });
            picked
        }
        SettingKind::File | SettingKind::Directory => super::controls::path(ui, entry, label),
        SettingKind::Colour => super::controls::colour(ui, entry, label),
        SettingKind::Date | SettingKind::Time | SettingKind::DateTime => {
            super::controls::when(ui, entry, label)
        }
        SettingKind::Flags => super::controls::flags(ui, entry, label),
        SettingKind::Key => super::controls::key(ui, entry, label),
        // Text, and any kind a newer loader adds: every value is text.
        _ => {
            let mut text = current.to_owned();
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(140.0));
                ui.label(label);
                r
            })
            .inner
            .lost_focus()
            .then_some(text)
            .filter(|t| t != current)
        }
    }
}
