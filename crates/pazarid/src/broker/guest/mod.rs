use std::{collections::HashMap, num::NonZeroU64};

use pazari_transport::PeerId;
use pazari_types::Timestamp;

use crate::broker::{
    arbiter::GuestView,
    phase::Negotiated,
    registry::{GuestId, GuestSpec, Registry},
    session::Session,
};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Guest {
    /// `None` until the guest's memory device has reported one.
    pub size: Option<u64>,

    pub reconcile: Option<Reconcile>,

    pub granted: Option<Timestamp>,

    pub session: Option<Session>,
}

impl Guest {
    #[must_use]
    pub const fn requested(&self) -> Option<u64> {
        match self.reconcile {
            Some(reconcile) => Some(reconcile.target),
            None => None,
        }
    }

    #[must_use]
    pub fn view<'a>(&self, id: GuestId, spec: &'a GuestSpec) -> GuestView<'a> {
        let negotiated = self.session.as_ref().and_then(Session::negotiated);

        GuestView {
            id,
            spec,
            boot_mem: negotiated.map(|session| session.boot_mem),
            size: self.size.unwrap_or(0),
            requested: self.requested(),
            status: negotiated.and_then(|session| session.status),
            stale: negotiated.is_some_and(|session| session.stale),
            connected: negotiated.is_some(),
            granted: self.granted,
        }
    }
}

/// An actuation the broker has asked for and not yet seen land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reconcile {
    pub target: u64,

    /// The size the guest had when the actuation started.
    pub from: u64,

    pub deadline: Timestamp,
}

impl Reconcile {
    #[must_use]
    pub const fn is_shrink(self) -> bool {
        self.target < self.from
    }

    #[must_use]
    pub const fn converged(self, size: u64) -> bool {
        if self.is_shrink() {
            size <= self.target
        } else {
            size >= self.target
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub spec: GuestSpec,
    pub state: Guest,
}

impl Entry {
    #[must_use]
    pub fn view(&self, id: GuestId) -> GuestView<'_> {
        self.state.view(id, &self.spec)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Guests {
    entries: Vec<Entry>,
    peers: HashMap<PeerId, GuestId>,
}

impl Guests {
    #[must_use]
    pub fn new(registry: Registry) -> Self {
        let (specs, peers) = registry.into_parts();

        Self {
            entries: specs
                .into_iter()
                .map(|spec| Entry {
                    spec,
                    state: Guest::default(),
                })
                .collect(),
            peers,
        }
    }

    #[must_use]
    pub fn lookup(&self, peer: &PeerId) -> Option<GuestId> {
        self.peers.get(peer).copied()
    }

    #[must_use]
    pub fn get(&self, id: GuestId) -> Option<&Entry> {
        self.entries.get(id.index())
    }

    pub fn get_mut(&mut self, id: GuestId) -> Option<&mut Entry> {
        self.entries.get_mut(id.index())
    }

    #[must_use]
    pub fn state(&self, id: GuestId) -> Option<&Guest> {
        self.get(id).map(|entry| &entry.state)
    }

    pub fn state_mut(&mut self, id: GuestId) -> Option<&mut Guest> {
        self.get_mut(id).map(|entry| &mut entry.state)
    }

    #[must_use]
    pub fn session(&self, id: GuestId) -> Option<&Session> {
        self.state(id)?.session.as_ref()
    }

    #[must_use]
    pub fn negotiated(&self, id: GuestId) -> Option<&Negotiated> {
        self.session(id)?.negotiated()
    }

    pub fn session_mut(&mut self, id: GuestId) -> Option<&mut Session> {
        self.state_mut(id)?.session.as_mut()
    }

    pub fn negotiated_mut(&mut self, id: GuestId) -> Option<&mut Negotiated> {
        self.session_mut(id)?.negotiated_mut()
    }

    #[must_use]
    pub fn ceiling(&self, id: GuestId) -> Option<NonZeroU64> {
        self.get(id).map(|entry| entry.spec.ceiling)
    }

    #[must_use]
    pub fn name(&self, id: GuestId) -> &str {
        self.get(id).map_or("?", |entry| entry.spec.name.as_str())
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn ids(&self) -> impl Iterator<Item = GuestId> {
        (0..self.entries.len()).map(GuestId::from_index)
    }

    pub fn iter(&self) -> impl Iterator<Item = (GuestId, &Entry)> {
        self.entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (GuestId::from_index(index), entry))
    }

    #[must_use]
    pub fn views(&self, asking: GuestId) -> (Option<GuestView<'_>>, Vec<GuestView<'_>>) {
        let mut mine = None;
        let mut others = Vec::with_capacity(self.len());

        for (id, entry) in self.iter() {
            let view = entry.view(id);

            if id == asking {
                mine = Some(view);
            } else {
                others.push(view);
            }
        }

        (mine, others)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::testing::{MIB, at};

    fn reconcile(from: u64, target: u64) -> Reconcile {
        Reconcile {
            target,
            from,
            deadline: at(30),
        }
    }

    #[test]
    fn a_grow_converges_once_the_size_reaches_the_target() {
        let growing = reconcile(2048 * MIB, 3072 * MIB);

        assert!(!growing.is_shrink());
        assert!(!growing.converged(2048 * MIB));
        assert!(!growing.converged(3071 * MIB));
        assert!(growing.converged(3072 * MIB));
        assert!(growing.converged(4096 * MIB));
    }

    #[test]
    fn a_shrink_converges_once_the_size_falls_to_the_target() {
        let shrinking = reconcile(3072 * MIB, 2048 * MIB);

        assert!(shrinking.is_shrink());
        assert!(!shrinking.converged(3072 * MIB));
        assert!(!shrinking.converged(2049 * MIB));
        assert!(shrinking.converged(2048 * MIB));
        assert!(shrinking.converged(1024 * MIB));
    }

    #[test]
    fn a_guest_with_nothing_in_flight_has_asked_for_nothing() {
        let mut guest = Guest::default();

        assert_eq!(guest.requested(), None);

        guest.reconcile = Some(reconcile(2048 * MIB, 3072 * MIB));

        assert_eq!(guest.requested(), Some(3072 * MIB));
    }
}
