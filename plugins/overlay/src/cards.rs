//! The showcase cards plugins set through the service (`card_set`), shown at the bottom
//! centre while an overlay window is open (the overlay draws nothing over the game
//! otherwise, so a card would get in the way of play there).
//!
//! Every change also goes out as a [`card_bus`] message, for a game's own UI that shows the
//! cards its own way. Once one says so (the `cards_elsewhere` setting), every card is sent
//! again, as it may have started after them, and the overlay draws none itself.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use uniloader_overlay::card_bus::{self, CardMessage};
use uniloader_overlay::showcase::{Showcase, ShownCard};
use uniloader_sdk::api::PluginHandle;

static SHOWCASE: Mutex<Option<Showcase>> = Mutex::new(None);
/// Set while a game's own UI shows the cards.
static ELSEWHERE: AtomicBool = AtomicBool::new(false);

fn with<R>(f: impl FnOnce(&mut Showcase) -> R) -> R {
    let mut guard: MutexGuard<'_, Option<Showcase>> =
        SHOWCASE.lock().unwrap_or_else(PoisonError::into_inner);
    f(guard.get_or_insert_with(Showcase::default))
}

fn publish(message: &CardMessage) {
    if let Some(host) = crate::host() {
        host.publish(card_bus::TOPIC, &message.encode());
    }
}

/// Adds or replaces a card.
pub(crate) fn set(card: ShownCard) {
    publish(&CardMessage::Set(card.clone()));
    with(|s| s.set(card));
}

/// Removes one card; `false` when the owner had none so named.
pub(crate) fn remove(owner: PluginHandle, name: &str) -> bool {
    let removed = with(|s| s.remove(owner, name));
    if removed {
        publish(&CardMessage::Remove(owner, name.to_owned()));
    }
    removed
}

/// Removes the cards of a plugin that stopped.
pub(crate) fn forget(owner: PluginHandle) {
    with(|s| s.forget(owner));
    publish(&CardMessage::Forget(owner));
}

/// A game's own UI shows the cards from now on (`true`), or no longer.
pub(crate) fn set_elsewhere(elsewhere: bool) {
    if elsewhere && !ELSEWHERE.swap(true, Ordering::Relaxed) {
        send_all();
    }
    ELSEWHERE.store(elsewhere, Ordering::Relaxed);
}

/// Sends every card again. Called when plugins are announced running: events published
/// while plugins are still starting reach nobody, and that is when most cards are set.
pub(crate) fn send_all() {
    for card in with(|s| s.cards().to_vec()) {
        publish(&CardMessage::Set(card));
    }
}

/// The card to draw now, if any; none while a game's own UI shows them.
pub(crate) fn current() -> Option<ShownCard> {
    // Read here too: a change made while the plugins were starting (a game UI claims the
    // cards in its own start) reaches no callback.
    let claimed = crate::host().is_some_and(|h| h.get("cards_elsewhere").as_deref() == Ok("true"));
    if ELSEWHERE.load(Ordering::Relaxed) || claimed {
        return None;
    }
    with(|s| s.current(Instant::now()).cloned())
}
