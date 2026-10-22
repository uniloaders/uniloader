//! Platform independent logic of uniloader.
//!
//! Nothing in here touches Windows, a game or a DLL, so it builds and tests on any host. The
//! `uniloader` crate wraps these types with the OS specific glue.
//!
//! Taken from the DayZ plugin loader (`dayz-plugin-loader`, `crates/dayz-plugin-core` at
//! `eaebd9b`), which this project replaces.

pub mod binds;
pub mod cmdline;
pub mod console;
pub mod deps;
pub mod facts;
pub mod keys;
pub mod names;
pub mod pe;
pub mod script;
pub mod settings;
pub mod store;
pub mod target;
pub mod thunk;
pub mod timescale;
pub mod values;
