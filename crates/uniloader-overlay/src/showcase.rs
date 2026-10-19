//! The cards behind [`crate::OverlayApi::card_set`], for providers: every plugin's cards in
//! the order they came, and which one is shown now. Each card stays up for
//! [`Showcase::DWELL`], then the next one comes; a plugin that stops loses its cards. No UI
//! here: a provider draws [`Showcase::current`] its own way.

use std::time::{Duration, Instant};

use uniloader_sdk::api::PluginHandle;

use crate::UiLevel;

/// One card, owned.
#[derive(Debug, Clone, PartialEq)]
pub struct ShownCard {
    /// The plugin that set it.
    pub owner: PluginHandle,
    /// Its name among that plugin's cards.
    pub name: String,
    /// A short tag on a coloured square, or empty.
    pub badge: String,
    /// Bold first line.
    pub title: String,
    /// The line under it.
    pub text: String,
    /// An image file shown instead of the badge square, or empty.
    pub icon: String,
    /// Colour of the badge square and the text line.
    pub level: UiLevel,
}

/// Every card, and the rotation.
#[derive(Debug, Default)]
pub struct Showcase {
    cards: Vec<ShownCard>,
    /// Index of the card shown, and since when.
    shown: Option<(usize, Instant)>,
    /// Bumped on every change, so a provider rebuilds its widgets only then.
    revision: u64,
}

impl Showcase {
    /// How long a card stays before the next one comes.
    pub const DWELL: Duration = Duration::from_secs(6);

    /// An empty showcase, usable in a `static`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            cards: Vec::new(),
            shown: None,
            revision: 0,
        }
    }

    /// Adds `card`, or replaces the owner's card of the same name in place.
    pub fn set(&mut self, card: ShownCard) {
        match self
            .cards
            .iter_mut()
            .find(|c| c.owner == card.owner && c.name == card.name)
        {
            Some(old) if *old == card => return,
            Some(old) => *old = card,
            None => self.cards.push(card),
        }
        self.revision += 1;
    }

    /// Removes the owner's card `name`; `false` when it had none so named.
    pub fn remove(&mut self, owner: PluginHandle, name: &str) -> bool {
        let before = self.cards.len();
        self.cards.retain(|c| !(c.owner == owner && c.name == name));
        self.changed(before)
    }

    /// Removes every card of a plugin that stopped.
    pub fn forget(&mut self, owner: PluginHandle) {
        let before = self.cards.len();
        self.cards.retain(|c| c.owner != owner);
        self.changed(before);
    }

    fn changed(&mut self, before: usize) -> bool {
        let removed = self.cards.len() != before;
        if removed {
            self.revision += 1;
            if let Some((i, since)) = self.shown {
                self.shown = (!self.cards.is_empty()).then(|| (i % self.cards.len(), since));
            }
        }
        removed
    }

    /// The card to show at `now`, moving on after [`Showcase::DWELL`]; `None` with no cards.
    pub fn current(&mut self, now: Instant) -> Option<&ShownCard> {
        if self.cards.is_empty() {
            self.shown = None;
            return None;
        }
        let (i, since) = match self.shown {
            Some((i, since)) if now.duration_since(since) < Self::DWELL => (i, since),
            Some((i, _)) => {
                self.revision += 1;
                ((i + 1) % self.cards.len(), now)
            }
            None => (0, now),
        };
        self.shown = Some((i, since));
        self.cards.get(i)
    }

    /// Changes whenever the card to draw may have changed (set, removed, rotated).
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// How many cards there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cards.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cards.is_empty()
    }

    /// Every card, in the order they came.
    #[must_use]
    pub fn cards(&self) -> &[ShownCard] {
        &self.cards
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(owner: u32, name: &str, title: &str) -> ShownCard {
        ShownCard {
            owner: PluginHandle(owner),
            name: name.to_owned(),
            badge: String::new(),
            title: title.to_owned(),
            text: String::new(),
            icon: String::new(),
            level: UiLevel::INFO,
        }
    }

    #[test]
    fn cards_rotate_after_the_dwell() {
        let mut s = Showcase::default();
        s.set(card(1, "a", "one"));
        s.set(card(2, "b", "two"));
        let t0 = Instant::now();
        assert_eq!(s.current(t0).map(|c| c.title.as_str()), Some("one"));
        assert_eq!(
            s.current(t0 + Duration::from_secs(1))
                .map(|c| c.title.as_str()),
            Some("one")
        );
        let t1 = t0 + Showcase::DWELL;
        assert_eq!(s.current(t1).map(|c| c.title.as_str()), Some("two"));
        assert_eq!(
            s.current(t1 + Showcase::DWELL).map(|c| c.title.as_str()),
            Some("one")
        );
    }

    #[test]
    fn set_replaces_by_owner_and_name() {
        let mut s = Showcase::default();
        s.set(card(1, "a", "old"));
        let rev = s.revision();
        s.set(card(1, "a", "old"));
        assert_eq!(s.revision(), rev, "an unchanged card is no change");
        s.set(card(1, "a", "new"));
        assert_eq!(s.len(), 1);
        assert_eq!(
            s.current(Instant::now()).map(|c| c.title.as_str()),
            Some("new")
        );
    }

    #[test]
    fn a_stopped_plugin_loses_its_cards() {
        let mut s = Showcase::default();
        s.set(card(1, "a", "one"));
        s.set(card(2, "b", "two"));
        s.set(card(1, "c", "three"));
        s.forget(PluginHandle(1));
        assert_eq!(s.len(), 1);
        assert!(!s.remove(PluginHandle(1), "a"));
        assert!(s.remove(PluginHandle(2), "b"));
        assert!(s.current(Instant::now()).is_none());
    }
}
