//! uniloader: a generic plugin loader for Windows games.
//!
//! Built as `uniloader.dll` and copied into the game directory under the name of a system DLL
//! the game imports (`dwmapi.dll`, `dxgi.dll`, `version.dll`, `dinput8.dll`, `winmm.dll`).
//! It forwards every export of that library to the real one in the system directory, and
//! hosts plugins found in `<game>/plugins/*.dll`; its own config and logs live in
//! `<game>/uniloader/`. Game knowledge belongs in adapter plugins, not here.
//!
//! The platform independent half (registries, key grammar, console parsing, config files)
//! lives in `uniloader-core`; this crate adds the state rules and, on Windows, the proxy and
//! plugin hosting. On other hosts only the platform independent modules compile, so the
//! workspace gate runs everywhere.

#![cfg_attr(not(windows), allow(dead_code))]

mod config;
mod console;
mod logging;
mod process;
mod scrollback;
mod state;

#[cfg(windows)]
mod win;

#[cfg(windows)]
mod dllmain;
