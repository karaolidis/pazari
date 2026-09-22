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

/// One guest as the operator declared it.
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Registry {
    specs: Vec<GuestSpec>,
    peers: HashMap<PeerId, GuestId>,
}

impl Registry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, peer: PeerId, spec: GuestSpec) -> Result<GuestId, RegistryError> {
        if let Some(id) = self.peers.get(&peer) {
            return Err(RegistryError::DuplicatePeer {
                peer,
                name: self.specs[id.index()].name.clone(),
            });
        }

        if self.specs.iter().any(|other| other.name == spec.name) {
            return Err(RegistryError::DuplicateName { name: spec.name });
        }

        let id = GuestId(self.specs.len());

        self.specs.push(spec);
        self.peers.insert(peer, id);

        Ok(id)
    }

    #[must_use]
    pub fn lookup(&self, peer: &PeerId) -> Option<GuestId> {
        self.peers.get(peer).copied()
    }

    #[must_use]
    pub fn spec(&self, id: GuestId) -> Option<&GuestSpec> {
        self.specs.get(id.index())
    }

    #[must_use]
    pub fn ceiling(&self, id: GuestId) -> Option<NonZeroU64> {
        self.spec(id).map(|spec| spec.ceiling)
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.specs.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    pub(crate) fn into_parts(self) -> (Vec<GuestSpec>, HashMap<PeerId, GuestId>) {
        (self.specs, self.peers)
    }

    pub fn iter(&self) -> impl Iterator<Item = (GuestId, &GuestSpec)> {
        self.specs
            .iter()
            .enumerate()
            .map(|(index, spec)| (GuestId(index), spec))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::testing::{CEILING, MIB, bytes, guest, unix, vsock};

    #[test]
    fn a_registry_answers_by_address() {
        let mut registry = Registry::new();

        let one = registry.insert(vsock(3), guest("one", CEILING)).unwrap();
        let two = registry.insert(vsock(4), guest("two", 512 * MIB)).unwrap();

        assert_eq!(registry.lookup(&vsock(3)), Some(one));
        assert_eq!(registry.lookup(&vsock(4)), Some(two));
        assert_eq!(registry.lookup(&vsock(5)), None);
        assert_eq!(registry.lookup(&unix("/run/pazari/one.sock")), None);

        assert_eq!(registry.ceiling(one), Some(bytes(CEILING)));
        assert_eq!(registry.ceiling(two), Some(bytes(512 * MIB)));
        assert_eq!(
            registry.spec(one).map(|spec| spec.name.as_str()),
            Some("one")
        );
        assert_eq!(registry.len(), 2);
        assert!(!registry.is_empty());
    }

    #[test]
    fn a_second_guest_at_one_address_is_refused() {
        let mut registry = Registry::new();

        registry.insert(vsock(3), guest("one", CEILING)).unwrap();

        assert_eq!(
            registry
                .insert(vsock(3), guest("two", CEILING))
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

        registry.insert(vsock(3), guest("one", CEILING)).unwrap();

        assert_eq!(
            registry
                .insert(vsock(4), guest("one", CEILING))
                .expect_err("one name, one guest"),
            RegistryError::DuplicateName {
                name: "one".to_owned(),
            }
        );
        assert_eq!(registry.lookup(&vsock(4)), None);
    }

    #[test]
    fn a_guest_outside_the_registry_has_no_ceiling() {
        let mut ours = Registry::new();
        let mut theirs = Registry::new();

        ours.insert(vsock(3), guest("one", CEILING)).unwrap();
        theirs.insert(vsock(9), guest("other", CEILING)).unwrap();
        let stranger = theirs
            .insert(vsock(10), guest("stranger", CEILING))
            .unwrap();

        assert_eq!(ours.ceiling(stranger), None);
        assert_eq!(ours.spec(stranger), None);
    }
}
