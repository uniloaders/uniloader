//! Which plugin is calling. Each plugin gets its own copy of the host table, whose opaque
//! `host` pointer (which the ABI says to pass back unchanged) names it; the host functions
//! that act in a plugin's name check that the handle passed is the caller's own. In-process
//! plugins are trusted, so this is about attribution: hooks, messages, services and input
//! claims stay with the plugin that made them, and so do the faults and teardown that follow.

use core::ffi::c_void;
use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use uniloader_api::{HostApi, PluginHandle};

/// A plugin's table. Raw pointers make it neither `Send` nor `Sync` on its own.
#[allow(unsafe_code)]
#[derive(Clone, Copy)]
struct Table(&'static HostApi);

// SAFETY: as for `super::SharedHost`: every pointer in the table is null, leaked `'static`
// storage or a function pointer, all usable from any thread.
#[allow(unsafe_code)]
unsafe impl Send for Table {}

/// Tables by plugin handle; one per handle, kept for reloads.
static TABLES: Mutex<BTreeMap<u32, Table>> = Mutex::new(BTreeMap::new());
/// Plugin handles by the address of their `host` context, compared, never dereferenced.
static CONTEXTS: Mutex<BTreeMap<usize, u32>> = Mutex::new(BTreeMap::new());

/// The host table for `plugin`: `base` with a `host` context of its own.
pub(super) fn table_for(plugin: PluginHandle, base: &'static HostApi) -> &'static HostApi {
    let mut tables = TABLES.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(table) = tables.get(&plugin.0) {
        return table.0;
    }
    let context: *mut c_void = Box::into_raw(Box::new(plugin.0)).cast();
    CONTEXTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(context as usize, plugin.0);
    let table: &'static HostApi = Box::leak(Box::new(HostApi {
        host: context,
        ..*base
    }));
    tables.insert(plugin.0, Table(table));
    table
}

/// Whether `host`, the context a caller passed back, is `plugin`'s. Says so in the log
/// when it is not.
pub(super) fn is(host: *mut c_void, plugin: PluginHandle, what: &str) -> bool {
    let owner = CONTEXTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&(host as usize))
        .copied();
    if owner == Some(plugin.0) {
        return true;
    }
    log::warn!(
        "{what}: refused, the handle {} is not the caller's (its context names {owner:?})",
        plugin.0
    );
    false
}
