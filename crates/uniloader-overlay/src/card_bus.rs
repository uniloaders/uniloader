//! Cards passed on from the overlay that provides the service to a game's own UI, which
//! shows them its own way (a game-styled card on its main menu). The provider publishes
//! every change on [`TOPIC`]; a game UI subscribes, keeps its own [`crate::showcase::Showcase`]
//! from the messages, and sets the provider's `cards_elsewhere` setting so the provider does
//! not draw them too.
//!
//! A message is its fields joined by NUL: `set`, owner, name, badge, title, text, icon,
//! level; `remove`, owner, name; or `forget`, owner.

use uniloader_sdk::api::PluginHandle;

use crate::showcase::ShownCard;
use crate::UiLevel;

/// The loader message topic the changes go out on.
pub const TOPIC: &str = "overlay.card";

/// One change to the cards.
#[derive(Debug, Clone, PartialEq)]
pub enum CardMessage {
    /// A card added or replaced.
    Set(ShownCard),
    /// One card of a plugin removed.
    Remove(PluginHandle, String),
    /// Every card of a plugin that stopped removed.
    Forget(PluginHandle),
}

const SEP: char = '\0';

impl CardMessage {
    /// The message as published.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let fields: Vec<String> = match self {
            Self::Set(c) => vec![
                "set".to_owned(),
                c.owner.0.to_string(),
                c.name.clone(),
                c.badge.clone(),
                c.title.clone(),
                c.text.clone(),
                c.icon.clone(),
                c.level.0.to_string(),
            ],
            Self::Remove(owner, name) => {
                vec!["remove".to_owned(), owner.0.to_string(), name.clone()]
            }
            Self::Forget(owner) => vec!["forget".to_owned(), owner.0.to_string()],
        };
        // A NUL inside a field would shift the others; it has no place in card text.
        let fields: Vec<String> = fields.into_iter().map(|f| f.replace(SEP, "")).collect();
        fields.join(&SEP.to_string()).into_bytes()
    }

    /// A published message; `None` when it is not one.
    #[must_use]
    pub fn decode(payload: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(payload).ok()?;
        let fields: Vec<&str> = text.split(SEP).collect();
        let owner = |i: usize| fields.get(i)?.parse::<u32>().ok().map(PluginHandle);
        match fields.first().copied()? {
            "set" if fields.len() == 8 => Some(Self::Set(ShownCard {
                owner: owner(1)?,
                name: fields[2].to_owned(),
                badge: fields[3].to_owned(),
                title: fields[4].to_owned(),
                text: fields[5].to_owned(),
                icon: fields[6].to_owned(),
                level: UiLevel(fields[7].parse().ok()?),
            })),
            "remove" if fields.len() == 3 => Some(Self::Remove(owner(1)?, fields[2].to_owned())),
            "forget" if fields.len() == 2 => Some(Self::Forget(owner(1)?)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_message_reads_back() {
        let card = ShownCard {
            owner: PluginHandle(7),
            name: "mp".to_owned(),
            badge: "MP".to_owned(),
            title: "tsw-coop".to_owned(),
            text: "multiplayer\0 loaded".to_owned(),
            icon: String::new(),
            level: UiLevel(2),
        };
        let set = CardMessage::decode(&CardMessage::Set(card.clone()).encode());
        let mut clean = card;
        clean.text = "multiplayer loaded".to_owned();
        assert_eq!(set, Some(CardMessage::Set(clean)));
        for m in [
            CardMessage::Remove(PluginHandle(7), "mp".to_owned()),
            CardMessage::Forget(PluginHandle(7)),
        ] {
            assert_eq!(CardMessage::decode(&m.encode()), Some(m));
        }
        assert_eq!(CardMessage::decode(b"nonsense"), None);
        assert_eq!(CardMessage::decode(b"set\x001"), None);
    }
}
