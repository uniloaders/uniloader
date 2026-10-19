//! A safe wrapper over [`OverlayApi`] for Rust plugins.
//!
//! Closures handed to the overlay are boxed and leaked: the overlay keeps the pointer for as
//! long as it might call it, and a plugin's DLL is never unloaded, so there is no moment at
//! which freeing it would be both safe and worth anything. A panic inside one is caught at
//! the boundary and logged; it never unwinds into the overlay.

// The consumer side of the service ABI; every unsafe block names the contract it relies on.
#![allow(unsafe_code)]

use core::ffi::c_void;
use core::marker::PhantomData;
use std::panic::{catch_unwind, AssertUnwindSafe};

use uniloader_sdk::api::{Caller, Status, Str};
use uniloader_sdk::{read_str, Host, PluginError, PluginRef};

use crate::abi::{
    CardDesc, DialogDesc, NoticeDesc, OverlayApi, PanelDesc, PresentInfo, UiAnswer, UiDialog,
    UiLevel, UiNotice, UiValue, UiWidget,
};

fn check(status: Status) -> Result<(), PluginError> {
    if status.is_ok() {
        Ok(())
    } else {
        Err(PluginError::Status(status))
    }
}

/// Run a consumer callback, keeping a panic on this side of the ABI.
fn contained(what: &str, f: impl FnOnce()) {
    if catch_unwind(AssertUnwindSafe(f)).is_err() {
        log::error!("overlay {what} callback panicked");
    }
}

/// One card for [`Overlay::card`].
#[derive(Debug, Clone, Copy)]
pub struct Card<'a> {
    /// A short tag on a coloured square (`MP`), or empty.
    pub badge: &'a str,
    /// Bold first line, usually the plugin's name.
    pub title: &'a str,
    /// The line under it.
    pub text: &'a str,
    /// An image file (PNG) shown instead of the badge square, or empty.
    pub icon: &'a str,
    /// Colour of the badge square and the text line.
    pub level: UiLevel,
}

/// The overlay, as one plugin sees it. Cheap to copy and usable from any thread.
#[derive(Clone, Copy)]
pub struct Overlay {
    api: &'static OverlayApi,
    /// This plugin's loader context, which tells the provider who is calling.
    caller: Caller,
    provider: PluginRef,
}

impl core::fmt::Debug for Overlay {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Overlay")
            .field("caller", &self.caller)
            .finish_non_exhaustive()
    }
}

/// What a panel callback needs to hand a [`Ui`] to the closure.
struct PanelCtx<F> {
    api: &'static OverlayApi,
    caller: Caller,
    draw: F,
}

/// What a dialog answer needs.
struct DialogCtx<F> {
    answer: F,
}

unsafe extern "C" fn draw_panel<F>(ctx: *mut c_void, _panel: Str, frame: u64)
where
    F: Fn(&mut Ui<'_>) + Send + Sync + 'static,
{
    if ctx.is_null() {
        return;
    }
    // SAFETY: `ctx` is the `PanelCtx<F>` leaked in `Overlay::panel` for this very function.
    let panel = unsafe { &*ctx.cast::<PanelCtx<F>>() };
    let mut ui = Ui {
        api: panel.api,
        caller: panel.caller,
        frame,
        _frame: PhantomData,
    };
    contained("panel", || (panel.draw)(&mut ui));
}

unsafe extern "C" fn answer_dialog<F>(ctx: *mut c_void, _id: u64, answer: UiAnswer, text: Str)
where
    F: FnOnce(UiAnswer, &str) + Send + 'static,
{
    if ctx.is_null() {
        return;
    }
    // SAFETY: `ctx` is the `DialogCtx<F>` leaked in `Overlay::dialog`, and the overlay calls
    // this exactly once, so taking ownership back is sound.
    let dialog = unsafe { Box::from_raw(ctx.cast::<DialogCtx<F>>()) };
    // SAFETY: `text` is valid for this call, as the ABI promises.
    let text = unsafe { read_str(text) };
    contained("dialog", || (dialog.answer)(answer, &text));
}

unsafe extern "C" fn on_present<F>(ctx: *mut c_void, info: *const PresentInfo)
where
    F: Fn(&PresentInfo) + Send + Sync + 'static,
{
    if ctx.is_null() || info.is_null() {
        return;
    }
    // SAFETY: `ctx` is the `F` leaked in `Overlay::on_present`; `info` is valid for the call.
    let (callback, info) = unsafe { (&*ctx.cast::<F>(), &*info) };
    contained("present", || callback(info));
}

impl Overlay {
    /// Find the overlay. `None` when no plugin provides it, or one with an incompatible
    /// layout does. Declare `Dependency::service(SERVICE)` so it is running by `start`.
    #[must_use]
    pub fn connect(host: &Host) -> Option<Overlay> {
        // SAFETY: `OverlayApi` is the layout the service `overlay` promises at this version;
        // the version and size are checked before the table is used.
        let service = unsafe { host.service::<OverlayApi>(crate::SERVICE) }?;
        if service.version != crate::VERSION
            || service.table.struct_size < core::mem::size_of::<OverlayApi>()
        {
            log::warn!(
                "overlay service v{} does not match the v{} this plugin was built for",
                service.version,
                crate::VERSION
            );
            return None;
        }
        Some(Overlay {
            api: service.table,
            caller: host.context(),
            provider: service.provider,
        })
    }

    /// The plugin providing the service: the egui `overlay`, or a game's own UI such as
    /// TSW's `tsw_ui`.
    #[must_use]
    pub fn provider(&self) -> PluginRef {
        self.provider
    }

    /// Add a panel, filled by `draw` each frame it is open.
    ///
    /// # Errors
    /// A bad or duplicate name.
    pub fn panel<F>(
        &self,
        name: &str,
        title: &str,
        default_open: bool,
        draw: F,
    ) -> Result<(), PluginError>
    where
        F: Fn(&mut Ui<'_>) + Send + Sync + 'static,
    {
        let ctx = Box::into_raw(Box::new(PanelCtx {
            api: self.api,
            caller: self.caller,
            draw,
        }));
        let desc = PanelDesc {
            struct_size: core::mem::size_of::<PanelDesc>(),
            name: Str::new(name),
            title: Str::new(title),
            default_open,
        };
        // SAFETY: `desc` outlives the call; `ctx` matches `draw_panel::<F>`.
        let status = unsafe {
            (self.api.panel_add)(
                self.caller,
                &raw const desc,
                Some(draw_panel::<F>),
                ctx.cast(),
            )
        };
        if !status.is_ok() {
            // SAFETY: refused, so the overlay kept no copy of the pointer.
            drop(unsafe { Box::from_raw(ctx) });
        }
        check(status)
    }

    /// Remove one of this plugin's panels.
    ///
    /// # Errors
    /// No such panel.
    pub fn remove_panel(&self, name: &str) -> Result<(), PluginError> {
        // SAFETY: plain values; `name` outlives the call.
        check(unsafe { (self.api.panel_remove)(self.caller, Str::new(name)) })
    }

    /// Open or close one of this plugin's panels.
    ///
    /// # Errors
    /// No such panel.
    pub fn set_open(&self, name: &str, open: bool) -> Result<(), PluginError> {
        // SAFETY: plain values; `name` outlives the call.
        check(unsafe { (self.api.panel_set_open)(self.caller, Str::new(name), open) })
    }

    /// Flip one of this plugin's panels.
    ///
    /// # Errors
    /// No such panel.
    pub fn toggle(&self, name: &str) -> Result<(), PluginError> {
        // SAFETY: plain values; `name` outlives the call.
        check(unsafe { (self.api.panel_toggle)(self.caller, Str::new(name)) })
    }

    /// Whether one of this plugin's panels is open; `false` for an unknown one.
    #[must_use]
    pub fn is_open(&self, name: &str) -> bool {
        let mut open = false;
        // SAFETY: `open` is a valid out-pointer and `name` outlives the call.
        let status =
            unsafe { (self.api.panel_is_open)(self.caller, Str::new(name), &raw mut open) };
        status.is_ok() && open
    }

    /// A card in the corner. Returns its id, for [`Overlay::close`].
    ///
    /// # Errors
    /// The overlay refused it.
    pub fn toast(&self, level: UiLevel, title: &str, text: &str) -> Result<u64, PluginError> {
        self.notice_of(UiNotice::TOAST, level, title, text, 0.0)
    }

    /// Large text in the middle of the screen for `seconds` (zero for the default).
    ///
    /// # Errors
    /// The overlay refused it.
    pub fn notice(
        &self,
        level: UiLevel,
        title: &str,
        text: &str,
        seconds: f32,
    ) -> Result<u64, PluginError> {
        self.notice_of(UiNotice::NOTICE, level, title, text, seconds)
    }

    /// Set (add or replace) this plugin's card `name` in the overlay's showcase, which cycles
    /// through the cards of every plugin (in TSW: the main menu's bottom centre).
    ///
    /// # Errors
    /// The overlay has no showcase ([`Status::UNSUPPORTED`]) or refused the card.
    pub fn card(&self, name: &str, card: &Card<'_>) -> Result<(), PluginError> {
        let desc = CardDesc {
            struct_size: core::mem::size_of::<CardDesc>(),
            badge: Str::new(card.badge),
            title: Str::new(card.title),
            text: Str::new(card.text),
            icon: Str::new(card.icon),
            level: card.level,
        };
        // SAFETY: `desc` and the strings it points to outlive the call.
        check(unsafe { (self.api.card_set)(self.caller, Str::new(name), &raw const desc) })
    }

    /// Take this plugin's card `name` out of the showcase.
    ///
    /// # Errors
    /// No such card, or no showcase.
    pub fn remove_card(&self, name: &str) -> Result<(), PluginError> {
        // SAFETY: `name` outlives the call.
        check(unsafe { (self.api.card_remove)(self.caller, Str::new(name)) })
    }

    fn notice_of(
        &self,
        kind: UiNotice,
        level: UiLevel,
        title: &str,
        text: &str,
        seconds: f32,
    ) -> Result<u64, PluginError> {
        let desc = NoticeDesc {
            struct_size: core::mem::size_of::<NoticeDesc>(),
            kind,
            level,
            title: Str::new(title),
            text: Str::new(text),
            seconds,
        };
        let mut id = 0u64;
        // SAFETY: `desc` and `id` outlive the call.
        check(unsafe { (self.api.notice_show)(self.caller, &raw const desc, &raw mut id) })?;
        Ok(id)
    }

    /// Open a modal dialog; `answer` runs once with how it ended (and the typed text for an
    /// accepted input dialog). `default_text` only matters for [`UiDialog::INPUT`].
    ///
    /// # Errors
    /// The overlay refused it.
    pub fn dialog<F>(
        &self,
        kind: UiDialog,
        level: UiLevel,
        title: &str,
        text: &str,
        default_text: &str,
        answer: F,
    ) -> Result<u64, PluginError>
    where
        F: FnOnce(UiAnswer, &str) + Send + 'static,
    {
        let ctx = Box::into_raw(Box::new(DialogCtx { answer }));
        let desc = DialogDesc {
            struct_size: core::mem::size_of::<DialogDesc>(),
            kind,
            level,
            title: Str::new(title),
            text: Str::new(text),
            default_text: Str::new(default_text),
            accept_label: Str::new(""),
            cancel_label: Str::new(""),
        };
        let mut id = 0u64;
        // SAFETY: `desc` and `id` outlive the call; `ctx` matches `answer_dialog::<F>`.
        let status = unsafe {
            (self.api.dialog_open)(
                self.caller,
                &raw const desc,
                Some(answer_dialog::<F>),
                ctx.cast(),
                &raw mut id,
            )
        };
        if !status.is_ok() {
            // SAFETY: refused, so the overlay kept no copy of the pointer.
            drop(unsafe { Box::from_raw(ctx) });
        }
        check(status)?;
        Ok(id)
    }

    /// Take down one of this plugin's toasts, notices or dialogs.
    ///
    /// # Errors
    /// No such id, or another plugin's.
    pub fn close(&self, id: u64) -> Result<(), PluginError> {
        // SAFETY: plain values.
        check(unsafe { (self.api.close)(self.caller, id) })
    }

    /// Run `callback` on the render thread before every present.
    ///
    /// # Errors
    /// The overlay refused it.
    pub fn on_present<F>(&self, callback: F) -> Result<(), PluginError>
    where
        F: Fn(&PresentInfo) + Send + Sync + 'static,
    {
        let ctx = Box::into_raw(Box::new(callback));
        // SAFETY: `ctx` matches `on_present::<F>`.
        let status =
            unsafe { (self.api.present_subscribe)(self.caller, Some(on_present::<F>), ctx.cast()) };
        if !status.is_ok() {
            // SAFETY: refused, so the overlay kept no copy of the pointer.
            drop(unsafe { Box::from_raw(ctx) });
        }
        check(status)
    }

    /// Whether the overlay has the keyboard and mouse right now.
    #[must_use]
    pub fn is_capturing(&self) -> bool {
        // SAFETY: no arguments.
        unsafe { (self.api.is_capturing)() }
    }
}

/// A panel body being filled. Only exists inside a panel's draw closure.
pub struct Ui<'frame> {
    api: &'static OverlayApi,
    caller: Caller,
    frame: u64,
    _frame: PhantomData<&'frame mut ()>,
}

impl Ui<'_> {
    fn widget(&mut self, kind: UiWidget, text: &str, value: &mut UiValue) -> Status {
        // SAFETY: `value` and `text` outlive the call, and `frame` is the token of the call
        // this `Ui` lives in.
        unsafe { (self.api.widget)(self.caller, self.frame, kind, Str::new(text), value) }
    }

    /// A line of text.
    pub fn label(&mut self, text: &str) {
        self.widget(UiWidget::LABEL, text, &mut UiValue::default());
    }

    /// A bigger line of text.
    pub fn heading(&mut self, text: &str) {
        self.widget(UiWidget::HEADING, text, &mut UiValue::default());
    }

    /// A horizontal rule.
    pub fn separator(&mut self) {
        self.widget(UiWidget::SEPARATOR, "", &mut UiValue::default());
    }

    /// Vertical space, in points; zero for the default.
    pub fn space(&mut self, points: f32) {
        let mut value = UiValue {
            float: points,
            ..UiValue::default()
        };
        self.widget(UiWidget::SPACE, "", &mut value);
    }

    /// A button; whether it was clicked this frame.
    pub fn button(&mut self, text: &str) -> bool {
        let mut value = UiValue::default();
        self.widget(UiWidget::BUTTON, text, &mut value).is_ok() && value.boolean
    }

    /// A checkbox over `on`; whether it changed.
    pub fn checkbox(&mut self, text: &str, on: &mut bool) -> bool {
        let mut value = UiValue {
            boolean: *on,
            ..UiValue::default()
        };
        self.widget(UiWidget::CHECKBOX, text, &mut value);
        let changed = value.boolean != *on;
        *on = value.boolean;
        changed
    }

    /// A slider over `v` in `min..=max`; whether it changed.
    pub fn slider(&mut self, text: &str, v: &mut f32, min: f32, max: f32) -> bool {
        let mut value = UiValue {
            float: *v,
            min: f64::from(min),
            max: f64::from(max),
            ..UiValue::default()
        };
        self.widget(UiWidget::SLIDER, text, &mut value);
        let changed = (value.float - *v).abs() > f32::EPSILON;
        *v = value.float;
        changed
    }

    /// An integer slider over `v` in `min..=max`; whether it changed.
    pub fn slider_int(&mut self, text: &str, v: &mut i64, min: i64, max: i64) -> bool {
        #[allow(
            clippy::cast_precision_loss,
            reason = "slider bounds, not exact values"
        )]
        let mut value = UiValue {
            integer: *v,
            min: min as f64,
            max: max as f64,
            ..UiValue::default()
        };
        self.widget(UiWidget::SLIDER_INT, text, &mut value);
        let changed = value.integer != *v;
        *v = value.integer;
        changed
    }

    /// The control for a registered setting: `key` for this plugin's own, `<plugin>.<key>`
    /// for another's. Changes are written through the loader.
    pub fn setting(&mut self, name: &str) {
        self.widget(UiWidget::SETTING, name, &mut UiValue::default());
    }

    /// A labelled row of `count` checkboxes over the bits of `bits`; whether any changed.
    /// `label_width` lines the boxes up across rows (zero for the default).
    pub fn toggles(&mut self, text: &str, bits: &mut i64, count: u32, label_width: f32) -> bool {
        let mut value = UiValue {
            integer: *bits,
            min: f64::from(label_width),
            max: f64::from(count),
            ..UiValue::default()
        };
        self.widget(UiWidget::TOGGLES, text, &mut value);
        let changed = value.integer != *bits;
        *bits = value.integer;
        changed
    }
}
