//! One frame of the overlay, independent of any renderer: input in, egui output out.
//!
//! The renderer (`win`) owns the device and calls [`Gui::run`] from inside `Present`; this
//! side decides what is drawn. Anything that calls back into a plugin — a typed console line,
//! a setting written from a widget, a dialog answer — waits until the egui pass is over, so
//! no plugin is ever called while another plugin's panel body is on the stack.

mod browser;
pub(crate) mod calls;
mod chrome;
mod console;
mod controls;
pub(crate) mod frame;
mod icons;
pub(crate) mod native;
mod paint;
mod recorder;
mod settings_panel;

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use egui::{Context, Pos2, Rect, Vec2};
use uniloader_core::keys::Modifiers;
use uniloader_sdk::Host;

use crate::layout::{self, Geometry};
use crate::notices::{self, Answered};
use crate::{registry, shared};

/// Id of the console window in the layout.
const CONSOLE_ID: &str = "overlay.console";
/// Id of the settings editor in the layout.
const EDITOR_ID: &str = "overlay.settings";
/// How often the hint's binding is looked up again.
const HINT_REFRESH: Duration = Duration::from_secs(2);

/// Dialog answers waiting for the next frame.
static ANSWERS: Mutex<Vec<Answered>> = Mutex::new(Vec::new());

fn answers() -> MutexGuard<'static, Vec<Answered>> {
    ANSWERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Deliver a dialog answer after the current frame rather than now.
pub(crate) fn queue_answer(answered: Answered) {
    answers().push(answered);
}

/// One frame of input, as the platform layer collected it.
pub(crate) struct FrameInput {
    /// egui's input: events, screen, time.
    pub raw: egui::RawInput,
    /// Where the virtual pointer is, for the overlay's own cursor.
    pub pointer: Pos2,
    /// The last key pressed (virtual key, scan code, modifiers), for the hotkey recorder.
    pub pressed: Option<(u16, u16, Modifiers)>,
}

/// What a pass decided, applied once it is over.
#[derive(Default)]
struct After {
    submitted: Option<String>,
    closed_panels: Vec<String>,
    placements: Vec<(String, Geometry)>,
    edits: settings_panel::Edits,
    /// A hotkey binding the recorder finished: action and binding.
    recorded: Option<(String, String)>,
    painted: paint::Outcome,
}

/// Everything the overlay keeps between frames.
pub(crate) struct Gui {
    ctx: Context,
    console: console::ConsolePanel,
    /// Whether the console was drawn last frame, so opening it can take the keyboard.
    console_shown: bool,
    scale: f32,
    /// `overlay.mouse`'s binding for the hint, and when it was read.
    mouse_binding: (Instant, String),
}

impl Gui {
    /// A fresh overlay in the dark theme.
    pub(crate) fn new() -> Self {
        let ctx = Context::default();
        ctx.set_theme(egui::Theme::Dark);
        Gui {
            ctx,
            console: console::ConsolePanel::default(),
            console_shown: false,
            scale: 1.0,
            mouse_binding: (Instant::now(), String::new()),
        }
    }

    /// The egui context, for the renderer.
    pub(crate) fn context(&self) -> &Context {
        &self.ctx
    }

    /// Whether a binding wants the next key press.
    #[allow(clippy::unused_self, reason = "the renderer asks the GUI it owns")]
    pub(crate) fn recording(&self) -> bool {
        recorder::is_recording()
    }

    /// Match egui's scale to the backbuffer, so a 4K window does not get a 1080p-sized UI.
    pub(crate) fn rescale(&mut self, height: f32) {
        let wanted = (height / 1080.0).clamp(1.0, 2.5);
        if (wanted - self.scale).abs() > 0.01 {
            self.ctx.set_pixels_per_point(wanted);
            self.scale = wanted;
        }
    }

    /// Run one frame and apply what it decided.
    pub(crate) fn run(&mut self, host: &Host, input: FrameInput) -> egui::FullOutput {
        let console_open = shared::console_open();
        if console_open {
            if !self.console_shown {
                self.console.opened(host);
            }
            self.console.pull(host);
        }
        self.console_shown = console_open;
        let mut after = After::default();
        let output = self.build(host, input, &mut after);
        self.apply(host, after);
        output
    }

    fn build(&mut self, host: &Host, input: FrameInput, after: &mut After) -> egui::FullOutput {
        let panels = registry::snapshot();
        let editor_open = shared::editor_open();
        let sections = if editor_open {
            editor_sections(host)
        } else {
            Vec::new()
        };
        let advanced = layout::with(|l| l.advanced());
        let queued = notices::snapshot();
        let windows_open = shared::console_open() || editor_open || panels.iter().any(|p| p.open);
        let card = crate::cards::current();
        let hint = self.hint(host);
        let ctx = self.ctx.clone();
        let FrameInput {
            raw,
            pointer,
            pressed,
        } = input;
        if let Some((vk, scancode, modifiers)) = pressed {
            match recorder::press(vk, scancode, modifiers) {
                Some((recorder::Target::Hotkey(action), binding)) => {
                    after.recorded = Some((action, binding));
                }
                Some((recorder::Target::Setting(name), binding)) => {
                    frame::queue_set(name, binding);
                }
                None => {}
            }
        }
        if !windows_open {
            // Nothing to record into or browse for.
            recorder::stop();
            browser::close();
        }
        ctx.run_ui(raw, |ui| {
            let ctx = ui.ctx().clone();
            if shared::console_open() {
                let chrome = chrome::Chrome {
                    id: CONSOLE_ID,
                    title: "uniloader console",
                    default_size: [720.0, 420.0],
                    saved: saved(CONSOLE_ID),
                };
                let outcome = self.console.show(&ctx, &chrome);
                after.submitted = outcome.submitted;
                place(&mut after.placements, CONSOLE_ID, &outcome.placed);
                if !outcome.placed.open || outcome.close {
                    shared::close_console();
                }
            }
            for panel in panels.iter().filter(|p| p.open) {
                Self::panel(host, &ctx, panel, after);
            }
            if editor_open {
                let chrome = chrome::Chrome {
                    id: EDITOR_ID,
                    title: "Settings",
                    default_size: [560.0, 420.0],
                    saved: saved(EDITOR_ID),
                };
                let view = settings_panel::View {
                    sections: &sections,
                    advanced,
                };
                let (edits, placed) = settings_panel::show(&ctx, &view, &chrome);
                after.edits = edits;
                place(&mut after.placements, EDITOR_ID, &placed);
                if !placed.open {
                    shared::close_editor();
                }
            }
            browser::show(&ctx);
            after.painted = paint::draw(&ctx, &queued);
            if windows_open {
                if let Some(card) = &card {
                    paint::card(&ctx, card);
                }
            }
            if shared::is_capturing() {
                cursor(&ctx, pointer);
            } else if shared::panels_open() {
                released(&ctx, &hint);
            }
        })
    }

    /// Draw one plugin panel, its body filled by the owner.
    fn panel(host: &Host, ctx: &Context, panel: &registry::Panel, after: &mut After) {
        let chrome = chrome::Chrome {
            id: &panel.id,
            title: &panel.title,
            default_size: [320.0, 240.0],
            saved: saved(&panel.id),
        };
        let token = frame::next_token();
        let placed = chrome::show(ctx, &chrome, |ui| {
            native::panel(ui, &panel.id, |ui| {
                if !calls::panel(host, panel, ui, token) {
                    ui.label("This panel's plugin is not running.");
                }
            });
        });
        place(&mut after.placements, &panel.id, &placed);
        if !placed.open {
            after.closed_panels.push(panel.id.clone());
        }
    }

    /// The binding that takes the mouse back, looked up every few seconds.
    fn hint(&mut self, host: &Host) -> String {
        let (read, binding) = &self.mouse_binding;
        if !binding.is_empty() && read.elapsed() < HINT_REFRESH {
            return binding.clone();
        }
        let binding = host
            .hotkeys()
            .into_iter()
            .find(|h| h.name == "overlay.mouse")
            .map_or_else(|| "none".to_owned(), |h| h.binding);
        self.mouse_binding = (Instant::now(), binding.clone());
        binding
    }

    /// Apply what the pass decided, with no frame on the stack.
    fn apply(&mut self, host: &Host, after: After) {
        for id in &after.closed_panels {
            registry::close_id(id);
        }
        if !after.closed_panels.is_empty() {
            shared::refresh();
        }
        let pointer_down = self.ctx.input(|i| i.pointer.any_down());
        layout::with(|l| {
            for (id, geometry) in &after.placements {
                l.remember(id, *geometry);
            }
            if let Some(advanced) = after.edits.advanced {
                l.set_advanced(advanced);
            }
        });
        // Once the mouse is let go: that is when a drag has decided where the window goes.
        if !pointer_down {
            layout::save();
        }
        let writes = after
            .edits
            .settings
            .into_iter()
            .chain(frame::take_pending());
        for (name, value) in writes {
            if let Err(e) = host.set(&name, &value) {
                log::warn!("could not set {name} = {value}: {e}");
            }
        }
        for (action, binding) in after.edits.bindings.iter().chain(&after.recorded) {
            match host.rebind(action, binding) {
                Ok(()) => log::info!("rebound {action} to {binding:?}"),
                Err(e) => log::warn!("could not rebind {action}: {e}"),
            }
        }
        resolve(host, &after.painted);
        if let Some(line) = after.submitted {
            // Not here: this is the render thread, which must not wait for a busy console
            // nor run a `plugin load` or a script inline.
            host.console_post(&line);
            self.console.ran();
        }
    }
}

/// Apply what one frame of toasts, notices and dialogs decided, and answer dialogs.
fn resolve(host: &Host, painted: &paint::Outcome) {
    for (id, text) in &painted.typed {
        notices::set_input(*id, text);
    }
    for (id, ticked) in &painted.dont_show {
        notices::set_dont_show(*id, *ticked);
    }
    let config_dir = host.config_dir();
    let mut answered: Vec<Answered> = painted
        .answered
        .iter()
        .filter_map(|(id, answer)| notices::take(*id, *answer, Some(&config_dir)))
        .collect();
    answered.append(&mut answers());
    notices::mark_shown(&painted.shown);
    shared::recount();
    for answer in &answered {
        calls::answer(host, answer);
    }
}

/// The settings editor's view of the registries, one section per plugin.
fn editor_sections(host: &Host) -> Vec<settings_panel::Section> {
    let plugins = host
        .modules()
        .into_iter()
        .filter(|m| m.plugin.is_some())
        .collect();
    settings_panel::sections(plugins, host.settings(None), host.hotkeys())
}

/// Where a window was left.
fn saved(id: &str) -> Option<Geometry> {
    layout::with(|l| l.get(id))
}

/// Note where a window ended up, when it had a rectangle this frame.
fn place(into: &mut Vec<(String, Geometry)>, id: &str, placed: &chrome::Placed) {
    if let Some(geometry) = placed.geometry {
        into.push((id.to_owned(), geometry));
    }
}

/// Draw the overlay's own pointer: the game hides and recentres the system cursor while it
/// has the mouse, so the only reliable cursor while a window is open is one drawn here.
fn cursor(ctx: &Context, position: Pos2) {
    let at = position / ctx.pixels_per_point();
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("overlay-cursor"),
    ));
    let arrow = vec![
        at,
        at + Vec2::new(0.0, 16.0),
        at + Vec2::new(4.5, 11.5),
        at + Vec2::new(11.0, 11.0),
    ];
    painter.add(egui::Shape::convex_polygon(
        arrow,
        egui::Color32::WHITE,
        egui::Stroke::new(1.0, egui::Color32::BLACK),
    ));
}

/// Say that the game has the input, and which key takes it back.
fn released(ctx: &Context, binding: &str) {
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("overlay-released"),
    ));
    let at = Pos2::new(ctx.viewport_rect().center().x, 12.0);
    let galley = painter.layout_no_wrap(
        format!("The game has the mouse — {binding} to take it back"),
        egui::FontId::proportional(13.0),
        egui::Color32::from_gray(210),
    );
    let rect = Rect::from_center_size(
        Pos2::new(at.x, at.y + galley.size().y / 2.0),
        galley.size() + Vec2::new(16.0, 8.0),
    );
    painter.rect_filled(rect, 4.0, egui::Color32::from_black_alpha(160));
    painter.galley(rect.min + Vec2::new(8.0, 4.0), galley, egui::Color32::WHITE);
}
