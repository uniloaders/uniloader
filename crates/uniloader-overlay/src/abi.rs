//! The `#[repr(C)]` layout of the service. Every function takes the caller's loader context
//! (`Host::context`) as `caller`; the provider asks the loader whose it is
//! (`Host::plugin_of`), files what it is asked for under that plugin, takes it all down when
//! that plugin stops, and calls back into it under the loader's fault guard. A plugin cannot
//! act in another's name.

use core::ffi::c_void;

use uniloader_sdk::api::{Caller, Status, Str};

/// How important a toast, notice or dialog is, which decides its colour.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UiLevel(pub u32);

impl UiLevel {
    /// Neutral.
    pub const INFO: UiLevel = UiLevel(0);
    /// Something worked.
    pub const SUCCESS: UiLevel = UiLevel(1);
    /// Something needs attention but nothing failed.
    pub const WARNING: UiLevel = UiLevel(2);
    /// Something failed.
    pub const ERROR: UiLevel = UiLevel(3);
}

/// Which kind of passing message [`OverlayApi::notice_show`] puts up.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiNotice(pub u32);

impl UiNotice {
    /// A small card in the corner, stacked under the ones already there.
    pub const TOAST: UiNotice = UiNotice(0);
    /// Large text in the middle of the screen, for the few things that must be seen.
    pub const NOTICE: UiNotice = UiNotice(1);
}

/// Which kind of dialog [`OverlayApi::dialog_open`] puts up. All are modal: while one is
/// open the overlay has the keyboard and mouse.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiDialog(pub u32);

impl UiDialog {
    /// Text and one button.
    pub const MESSAGE: UiDialog = UiDialog(0);
    /// Text, a confirming button and a cancelling one.
    pub const CONFIRM: UiDialog = UiDialog(1);
    /// Text and one line to type into.
    pub const INPUT: UiDialog = UiDialog(2);
    /// An [`UiDialog::INPUT`] whose line is masked (a password); the text is never logged.
    pub const SECRET: UiDialog = UiDialog(3);

    /// Whether it has a line to type into ([`UiDialog::INPUT`], [`UiDialog::SECRET`]).
    #[must_use]
    pub fn takes_text(self) -> bool {
        self == Self::INPUT || self == Self::SECRET
    }
}

/// How a dialog ended.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiAnswer(pub u32);

impl UiAnswer {
    /// The confirming button, or Enter in an input dialog.
    pub const ACCEPTED: UiAnswer = UiAnswer(0);
    /// The cancelling button.
    pub const CANCELLED: UiAnswer = UiAnswer(1);
    /// Taken down by [`OverlayApi::close`].
    pub const CLOSED: UiAnswer = UiAnswer(2);
    /// Not shown: the user ticked "Don't show again" on a dialog with the same
    /// [`DialogDesc::remember`] key before.
    pub const SUPPRESSED: UiAnswer = UiAnswer(3);
    /// Gone without a choice: Escape, Back, the window's close button, or a provider that
    /// lost or could not show it. A dialog whose cancel button does something (not only
    /// "no") takes this as "decide later"; for the others it is the same as
    /// [`UiAnswer::CANCELLED`].
    pub const DISMISSED: UiAnswer = UiAnswer(4);
}

/// Which widget [`OverlayApi::widget`] adds.
///
/// One entry point rather than one function per widget: a new widget costs a consumer that
/// does not know it nothing, where a new function would change the table.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiWidget(pub u32);

impl UiWidget {
    /// A line of text. Uses `text`.
    pub const LABEL: UiWidget = UiWidget(0);
    /// A bigger line of text. Uses `text`.
    pub const HEADING: UiWidget = UiWidget(1);
    /// A horizontal rule.
    pub const SEPARATOR: UiWidget = UiWidget(2);
    /// Vertical space of `value.float` points, or a default when it is zero.
    pub const SPACE: UiWidget = UiWidget(3);
    /// A button. Sets `value.boolean` to whether it was clicked this frame.
    pub const BUTTON: UiWidget = UiWidget(4);
    /// A checkbox over `value.boolean`.
    pub const CHECKBOX: UiWidget = UiWidget(5);
    /// A slider over `value.float`, bounded by `value.min` and `value.max`.
    pub const SLIDER: UiWidget = UiWidget(6);
    /// A slider over `value.integer`, bounded by `value.min` and `value.max`.
    pub const SLIDER_INT: UiWidget = UiWidget(7);
    /// The control for a registered setting named by `text` (`key` for the owner's own,
    /// `<plugin>.<key>` for another's). The overlay reads the descriptor, draws the matching
    /// control and writes changes through the loader, which validates, persists and
    /// notifies. Ignores `value`.
    pub const SETTING: UiWidget = UiWidget(8);
    /// A labelled row of checkboxes, one per bit of `value.integer`. `value.max` is how many
    /// (1 to 32), `value.min` the label column width in points (0 for the default).
    pub const TOGGLES: UiWidget = UiWidget(9);
    /// One row of a list, highlighted when `value.boolean` is set on the way in; sets
    /// `value.boolean` to whether it was clicked this frame. Uses `text`.
    pub const SELECTABLE: UiWidget = UiWidget(10);
    /// An engine texture beside the panel (a preview of what the panel picks): `value.integer`
    /// is the texture object's address in the host engine, `value.min` × `value.max` its
    /// size in points. Only a provider drawing with that engine can show one; the others
    /// return `UNSUPPORTED`. Uses `text` as the caption.
    pub const TEXTURE: UiWidget = UiWidget(11);
    /// A texture of the game's graphics device in the panel (a preview the plugin renders):
    /// `value.integer` is its native handle (an `ID3D12Resource*` under `Direct3D` 12),
    /// `value.min` × `value.max` its size in points, `text` a caption. The caller keeps
    /// the texture alive for the call (holding a reference of its own across it, and
    /// never letting it go on another thread meanwhile); the overlay takes its own
    /// reference during the call and touches the handle at no other time. A provider that cannot draw that device's
    /// textures returns `UNSUPPORTED`. The texture holds gamma-encoded, premultiplied RGBA.
    pub const GPU_TEXTURE: UiWidget = UiWidget(12);
    /// What the widget added just before does, `text` (a button's description): a provider
    /// shows it in its description pane, as hover text or as a small line under the widget.
    /// Ignores `value`.
    pub const TIP: UiWidget = UiWidget(13);
}

/// The value a widget reads, writes, or both. Fields a widget does not use are ignored.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct UiValue {
    /// `size_of::<UiValue>()`.
    pub struct_size: usize,
    /// Boolean in and out.
    pub boolean: bool,
    /// Float in and out.
    pub float: f32,
    /// Integer in and out.
    pub integer: i64,
    /// Lower bound for the slider widgets.
    pub min: f64,
    /// Upper bound for the slider widgets.
    pub max: f64,
}

impl Default for UiValue {
    fn default() -> Self {
        UiValue {
            struct_size: core::mem::size_of::<UiValue>(),
            boolean: false,
            float: 0.0,
            integer: 0,
            min: 0.0,
            max: 0.0,
        }
    }
}

/// A panel: a window the overlay draws and the plugin fills.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PanelDesc {
    /// `size_of::<PanelDesc>()`.
    pub struct_size: usize,
    /// Name within the owner, `[a-z0-9_]+`. The window is remembered as `<plugin>.<name>`.
    pub name: Str,
    /// Window title.
    pub title: Str,
    /// Whether it starts open.
    pub default_open: bool,
}

/// A toast or a notice. Neither takes input; both disappear by themselves.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NoticeDesc {
    /// `size_of::<NoticeDesc>()`.
    pub struct_size: usize,
    /// Toast or notice.
    pub kind: UiNotice,
    /// Colour.
    pub level: UiLevel,
    /// Bold first line, or empty.
    pub title: Str,
    /// The message.
    pub text: Str,
    /// Seconds on screen; zero for the default.
    pub seconds: f32,
}

/// A card in the overlay's shared showcase: a small, always-visible element (the main menu's
/// bottom centre in TSW) that cycles through the cards of every plugin that set one. For
/// "this mod is loaded", news, tips. It takes no input.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CardDesc {
    /// `size_of::<CardDesc>()`.
    pub struct_size: usize,
    /// A short tag on a coloured square (`MP`), or empty.
    pub badge: Str,
    /// Bold first line, usually the plugin's name.
    pub title: Str,
    /// The line under it.
    pub text: Str,
    /// An image file (PNG) shown instead of the badge square, or empty.
    pub icon: Str,
    /// Colour of the badge square and the text line.
    pub level: UiLevel,
}

/// A modal dialog.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DialogDesc {
    /// `size_of::<DialogDesc>()`.
    pub struct_size: usize,
    /// Message, confirm or input.
    pub kind: UiDialog,
    /// Colour.
    pub level: UiLevel,
    /// Window title.
    pub title: Str,
    /// Body text.
    pub text: Str,
    /// Starting content of the input line.
    pub default_text: Str,
    /// Accepting button label, or empty for the default.
    pub accept_label: Str,
    /// Cancelling button label, or empty for the default.
    pub cancel_label: Str,
    /// A key for "Don't show again", or empty for a dialog without that checkbox. Once the
    /// user ticks it and closes the dialog, every later dialog of the same plugin with the
    /// same key answers [`UiAnswer::SUPPRESSED`] at once instead of showing. Put whatever
    /// makes the message new into the key, such as `outdated/1.2.0`, so a new version is
    /// shown again. Kept in `uniloader/config/dont-show-again.txt` (see [`crate::remember`]).
    pub remember: Str,
}

/// The graphics API a swapchain belongs to.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphicsApi(pub u32);

impl GraphicsApi {
    /// A DXGI swapchain: `Direct3D` 10, 11 or 12.
    pub const DXGI: GraphicsApi = GraphicsApi(1);
    /// `OpenGL` through WGL: called from `wglSwapBuffers` with the game's context current.
    pub const OPENGL: GraphicsApi = GraphicsApi(2);
    /// Vulkan: called from `vkQueuePresentKHR`, before the overlay's own submission.
    pub const VULKAN: GraphicsApi = GraphicsApi(3);
}

/// One frame about to be presented.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PresentInfo {
    /// `size_of::<PresentInfo>()`.
    pub struct_size: usize,
    /// Which API `swapchain` and `device` belong to.
    pub api: GraphicsApi,
    /// For DXGI, the `IDXGISwapChain*` being presented. Borrowed for the call. For `OpenGL`,
    /// the `HDC` passed to `wglSwapBuffers`. For Vulkan, the `VkSwapchainKHR` (the first
    /// one of the present).
    pub swapchain: *mut c_void,
    /// For DXGI, the `ID3D11Device*` behind it, or null when it is not a `Direct3D` 11 one.
    /// For `OpenGL`, the current `HGLRC`. For Vulkan, the swapchain's `VkDevice`.
    pub device: *mut c_void,
    /// The window it presents to; null for a Vulkan surface made before the overlay loaded.
    pub hwnd: *mut c_void,
    /// Backbuffer width in pixels.
    pub width: u32,
    /// Backbuffer height in pixels.
    pub height: u32,
    /// For DXGI, the sync interval the game passed to `Present`.
    pub sync_interval: u32,
    /// For DXGI, the `DXGI_PRESENT_*` flags the game passed to `Present`.
    pub flags: u32,
}

/// Fills a panel body: call [`OverlayApi::widget`] with `frame` for each widget.
pub type PanelFn = unsafe extern "C" fn(ctx: *mut c_void, panel: Str, frame: u64);
/// Receives a dialog's answer. `text` is what was typed into an accepted input dialog.
pub type DialogFn = unsafe extern "C" fn(ctx: *mut c_void, id: u64, answer: UiAnswer, text: Str);
/// Runs on the render thread just before a frame is presented, before the overlay draws.
pub type PresentFn = unsafe extern "C" fn(ctx: *mut c_void, info: *const PresentInfo);

/// The service table, version [`crate::VERSION`]. Every function is thread safe; `widget`
/// only works inside the panel callback it belongs to.
#[repr(C)]
pub struct OverlayApi {
    /// `size_of::<OverlayApi>()` as compiled into the provider.
    pub struct_size: usize,
    /// Add a panel. `draw(ctx, ...)` fills it each frame it is open. Returns
    /// [`Status::ALREADY_EXISTS`] for a name the owner already used.
    pub panel_add: unsafe extern "C" fn(
        caller: Caller,
        desc: *const PanelDesc,
        draw: Option<PanelFn>,
        ctx: *mut c_void,
    ) -> Status,
    /// Remove one of the owner's panels.
    pub panel_remove: unsafe extern "C" fn(caller: Caller, name: Str) -> Status,
    /// Open or close one of the owner's panels.
    pub panel_set_open: unsafe extern "C" fn(caller: Caller, name: Str, open: bool) -> Status,
    /// Flip one of the owner's panels open or closed.
    pub panel_toggle: unsafe extern "C" fn(caller: Caller, name: Str) -> Status,
    /// Whether one of the owner's panels is open.
    pub panel_is_open: unsafe extern "C" fn(caller: Caller, name: Str, out: *mut bool) -> Status,
    /// Add a widget to the panel body being filled. `frame` is the token the draw callback
    /// received; outside that call it is refused with [`Status::WRONG_PHASE`] and nothing is
    /// dereferenced. A widget this overlay does not know is [`Status::UNSUPPORTED`].
    pub widget: unsafe extern "C" fn(
        caller: Caller,
        frame: u64,
        kind: UiWidget,
        text: Str,
        value: *mut UiValue,
    ) -> Status,
    /// Show a toast or notice; its id goes to `out_id`, which may be null.
    pub notice_show:
        unsafe extern "C" fn(caller: Caller, desc: *const NoticeDesc, out_id: *mut u64) -> Status,
    /// Open a modal dialog. `answer(ctx, ...)` is called exactly once, after the frame it was
    /// answered in — unless the owner stops first.
    pub dialog_open: unsafe extern "C" fn(
        caller: Caller,
        desc: *const DialogDesc,
        answer: Option<DialogFn>,
        ctx: *mut c_void,
        out_id: *mut u64,
    ) -> Status,
    /// Take down one of the owner's toasts, notices or dialogs early. A dialog answers
    /// [`UiAnswer::CLOSED`].
    pub close: unsafe extern "C" fn(caller: Caller, id: u64) -> Status,
    /// Be called on every present, on the render thread.
    pub present_subscribe: unsafe extern "C" fn(
        caller: Caller,
        callback: Option<PresentFn>,
        ctx: *mut c_void,
    ) -> Status,
    /// Whether the overlay has the keyboard and mouse right now.
    pub is_capturing: unsafe extern "C" fn() -> bool,
    /// Set (add or replace) the owner's card `name` in the showcase. A provider without a
    /// showcase answers [`Status::UNSUPPORTED`].
    pub card_set: unsafe extern "C" fn(caller: Caller, name: Str, desc: *const CardDesc) -> Status,
    /// Take the owner's card `name` out of the showcase.
    pub card_remove: unsafe extern "C" fn(caller: Caller, name: Str) -> Status,
}
