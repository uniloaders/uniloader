//! The showcase cards plugins set through the service (`card_set`), shown at the bottom
//! centre while an overlay window is open (the overlay draws nothing over the game
//! otherwise, so a card would get in the way of play there).

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use uniloader_overlay::showcase::{Showcase, ShownCard};
use uniloader_sdk::api::PluginHandle;

static SHOWCASE: Mutex<Option<Showcase>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Showcase) -> R) -> R {
    let mut guard: MutexGuard<'_, Option<Showcase>> =
        SHOWCASE.lock().unwrap_or_else(PoisonError::into_inner);
    f(guard.get_or_insert_with(Showcase::default))
}

/// Adds or replaces a card.
pub(crate) fn set(card: ShownCard) {
    with(|s| s.set(card));
}

/// Removes one card; `false` when the owner had none so named.
pub(crate) fn remove(owner: PluginHandle, name: &str) -> bool {
    with(|s| s.remove(owner, name))
}

/// Removes the cards of a plugin that stopped.
pub(crate) fn forget(owner: PluginHandle) {
    with(|s| s.forget(owner));
}

/// The card to draw now, if any.
pub(crate) fn current() -> Option<ShownCard> {
    with(|s| s.current(Instant::now()).cloned())
}
