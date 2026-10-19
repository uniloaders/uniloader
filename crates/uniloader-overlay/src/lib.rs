//! The `overlay` service: what the overlay plugin offers other plugins, and a safe client.
//!
//! The overlay is a plugin, not part of the loader, so that it can be replaced: anything that
//! provides the service `overlay` with the [`OverlayApi`] layout works for every consumer (the
//! egui `overlay` plugin, or a game's native UI such as TSW's `tsw_ui`). A consumer declares
//! `Dependency::service(SERVICE)`, then calls [`Overlay::connect`].
//!
//! The ABI never exposes the UI library. A plugin registers a panel and fills its body one
//! widget at a time through [`Ui`] while its draw callback runs; the overlay owns the window
//! chrome, the open state, the layout and the fault guard. That is what lets the same panel be
//! drawn by a different overlay — or onto a quad in VR — without the plugin changing.
//!
//! [`OverlayApi`] is the stable part. [`Overlay`] and [`Ui`] wrap it for Rust plugins; a
//! plugin in another language uses the table directly.

mod abi;
pub mod card_bus;
mod client;
pub mod showcase;

pub use abi::{
    CardDesc, DialogDesc, DialogFn, GraphicsApi, NoticeDesc, OverlayApi, PanelDesc, PanelFn,
    PresentFn, PresentInfo, UiAnswer, UiDialog, UiLevel, UiNotice, UiValue, UiWidget,
};
pub use client::{Card, Overlay, Ui};

/// Name the overlay registers its service under.
pub const SERVICE: &str = "overlay";
/// Version of [`OverlayApi`] this crate describes.
pub const VERSION: u32 = 1;
/// Name of the egui plugin that provides the service. Prefer a dependency on [`SERVICE`],
/// which any provider satisfies.
pub const PLUGIN: &str = "overlay";
