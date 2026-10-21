use std::{collections::HashMap, num::NonZeroU64};

use pazari_transport::PeerId;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GuestId(usize);

impl GuestId {
    #[must_use]
    pub const fn index(self) -> usize {
        self.0
    }

    pub(crate) const fn from_index(index: usize) -> Self {
        Self(index)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestSpec {
    pub name: String,

    pub ceiling: NonZeroU64,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RegistryError {
    #[error("{peer} is already registered as {name}")]
    DuplicatePeer { peer: PeerId, name: String },

    #[error("a guest named {name} is already registered")]
    DuplicateName { name: String },
}

/// A registered guest and what is kept alongside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry<T> {
    pub spec: GuestSpec,
    pub value: T,
}

/// The guests, each answering to one peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registry<T> {
    entries: Vec<Entry<T>>,
    peers: HashMap<PeerId, GuestId>,
}

impl<T> Default for Registry<T> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            peers: HashMap::new(),
        }
    }
}

impl<T> Registry<T> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(
        &mut self,
        peer: PeerId,
        spec: GuestSpec,
        value: T,
    ) -> Result<GuestId, RegistryError> {
        if let Some(id) = self.peers.get(&peer) {
            return Err(RegistryError::DuplicatePeer {
                peer,
                name: self.spec(*id).name.clone(),
            });
        }

        if self
            .entries
            .iter()
            .any(|other| other.spec.name == spec.name)
        {
            return Err(RegistryError::DuplicateName { name: spec.name });
        }

        let id = GuestId(self.entries.len());

        self.entries.push(Entry { spec, value });
        self.peers.insert(peer, id);

        Ok(id)
    }

    #[must_use]
    pub fn lookup(&self, peer: &PeerId) -> Option<GuestId> {
        self.peers.get(peer).copied()
    }

    #[must_use]
    pub fn get(&self, id: GuestId) -> &Entry<T> {
        &self.entries[id.index()]
    }

    pub fn get_mut(&mut self, id: GuestId) -> &mut Entry<T> {
        &mut self.entries[id.index()]
    }

    #[must_use]
    pub fn spec(&self, id: GuestId) -> &GuestSpec {
        &self.get(id).spec
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (GuestId, &Entry<T>)> {
        self.entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (GuestId(index), entry))
    }

    /// The same guests, each keeping `f` of what it kept.
    #[must_use]
    pub fn map<U>(self, mut f: impl FnMut(T) -> U) -> Registry<U> {
        Registry {
            entries: self
                .entries
                .into_iter()
                .map(|Entry { spec, value }| Entry {
                    spec,
                    value: f(value),
                })
                .collect(),
            peers: self.peers,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::testing::{CEILING, MIB, bytes, guest, unix, vsock};

    #[test]
    fn a_registry_answers_by_address() {
        let mut registry = Registry::new();

        let one = registry
            .insert(vsock(3), guest("one", CEILING), ())
            .unwrap();
        let two = registry
            .insert(vsock(4), guest("two", 512 * MIB), ())
            .unwrap();

        assert_eq!(registry.lookup(&vsock(3)), Some(one));
        assert_eq!(registry.lookup(&vsock(4)), Some(two));
        assert_eq!(registry.lookup(&vsock(5)), None);
        assert_eq!(registry.lookup(&unix("/run/pazari/one.sock")), None);

        assert_eq!(registry.spec(one).ceiling, bytes(CEILING));
        assert_eq!(registry.spec(two).ceiling, bytes(512 * MIB));
        assert_eq!(registry.spec(one).name, "one");
        assert_eq!(registry.len(), 2);
        assert!(!registry.is_empty());
    }

    #[test]
    fn a_second_guest_at_one_address_is_refused() {
        let mut registry = Registry::new();

        registry
            .insert(vsock(3), guest("one", CEILING), ())
            .unwrap();

        assert_eq!(
            registry
                .insert(vsock(3), guest("two", CEILING), ())
                .expect_err("one address, one guest"),
            RegistryError::DuplicatePeer {
                peer: vsock(3),
                name: "one".to_owned(),
            }
        );
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn a_second_guest_of_one_name_is_refused() {
        let mut registry = Registry::new();

        registry
            .insert(vsock(3), guest("one", CEILING), ())
            .unwrap();

        assert_eq!(
            registry
                .insert(vsock(4), guest("one", CEILING), ())
                .expect_err("one name, one guest"),
            RegistryError::DuplicateName {
                name: "one".to_owned(),
            }
        );
        assert_eq!(registry.lookup(&vsock(4)), None);
    }
}
