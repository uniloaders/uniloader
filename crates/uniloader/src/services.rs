//! Services plugins provide to each other: a function table under a name.
//!
//! The loader only keeps the pointer and who provided it. What the table contains is a
//! contract between the provider and its consumers, usually a small client crate; this is the
//! mechanism the adapters (an overlay, a game's engine) use to offer more than events.

use std::collections::BTreeMap;

use uniloader_api::{PluginHandle, Status};

/// One registered service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Service {
    /// The plugin that registered it.
    pub provider: PluginHandle,
    /// Version the provider declared.
    pub version: u32,
    /// The table's address. An integer, so the state stays `Send`; the table itself lives in
    /// the provider's DLL, which is never unloaded.
    pub table: usize,
}

/// Every service, by name.
#[derive(Debug, Default)]
pub struct Registry {
    services: BTreeMap<String, Service>,
}

/// Whether `name` is a valid service name: `[a-z0-9_.-]+`, at most 64 bytes.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b))
}

impl Registry {
    /// Register `service` under `name`.
    ///
    /// # Errors
    /// [`Status::INVALID_ARGUMENT`] for a bad name or a null table,
    /// [`Status::ALREADY_EXISTS`] when the name is taken.
    pub fn register(&mut self, name: &str, service: Service) -> Result<(), Status> {
        if !valid_name(name) || service.table == 0 {
            return Err(Status::INVALID_ARGUMENT);
        }
        if self.services.contains_key(name) {
            return Err(Status::ALREADY_EXISTS);
        }
        self.services.insert(name.to_owned(), service);
        Ok(())
    }

    /// The service registered under `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Service> {
        self.services.get(name).copied()
    }

    /// Every service with its name, for the `services` listing.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Service)> {
        self.services.iter().map(|(k, v)| (k.as_str(), v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(provider: u32) -> Service {
        Service {
            provider: PluginHandle(provider),
            version: 1,
            table: 0x1000,
        }
    }

    #[test]
    fn names_are_unique_and_checked() {
        let mut r = Registry::default();
        assert_eq!(r.register("overlay", service(1)), Ok(()));
        assert_eq!(
            r.register("overlay", service(2)),
            Err(Status::ALREADY_EXISTS)
        );
        assert_eq!(
            r.register("Overlay", service(2)),
            Err(Status::INVALID_ARGUMENT)
        );
        assert_eq!(r.register("", service(2)), Err(Status::INVALID_ARGUMENT));
        let null = Service {
            table: 0,
            ..service(2)
        };
        assert_eq!(
            r.register("ue4.objects", null),
            Err(Status::INVALID_ARGUMENT)
        );
        assert_eq!(r.register("ue4.objects", service(2)), Ok(()));
        assert_eq!(r.get("overlay").map(|s| s.provider), Some(PluginHandle(1)));
        assert_eq!(r.get("nope"), None);
        assert_eq!(r.iter().count(), 2);
    }
}
