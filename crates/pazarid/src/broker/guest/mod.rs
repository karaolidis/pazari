use std::{collections::HashMap, num::NonZeroU64};

use pazari_transport::PeerId;
use pazari_types::{Deadline, Timestamp};

use crate::broker::{
    arbiter::GuestView,
    phase::Negotiated,
    registry::{GuestId, GuestSpec, Registry},
    session::Session,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub size: u64,

    /// How much of that size was plugged in, rather than there at boot.
    pub hotplugged: u64,

    /// What the guest's kernel keeps for itself out of the device's size.
    ///
    /// The guest reads `MemTotal` as the size less this.
    pub offset: Option<u64>,
}

impl Geometry {
    const fn settled(size: u64, hotplugged: u64, offset: Option<u64>) -> Self {
        Self {
            size,
            hotplugged,
            offset,
        }
    }

    /// The memory the device held at boot.
    #[must_use]
    pub const fn boot(self) -> u64 {
        self.size.saturating_sub(self.hotplugged)
    }

    /// What the guest reads as `MemTotal` with the device at `size`.
    ///
    /// Until the offset is known this is the device's own figure.
    #[must_use]
    pub const fn seen(self, size: u64) -> u64 {
        match self.offset {
            Some(offset) => size.saturating_sub(offset),
            None => size,
        }
    }

    /// The device size at which the guest reads `seen` as `MemTotal`.
    ///
    /// Until the offset is known this is the guest's own figure.
    #[must_use]
    pub const fn device(self, seen: u64) -> u64 {
        match self.offset {
            Some(offset) => seen.saturating_add(offset),
            None => seen,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Guest {
    pub(crate) geometry: Option<Geometry>,

    pub(crate) reconcile: Option<Reconcile>,

    pub(crate) granted: Option<Timestamp>,

    pub(crate) session: Option<Session>,
}

impl Guest {
    #[must_use]
    pub const fn geometry(&self) -> Option<Geometry> {
        self.geometry
    }

    #[must_use]
    pub const fn size(&self) -> Option<u64> {
        match self.geometry {
            Some(geometry) => Some(geometry.size),
            None => None,
        }
    }

    #[must_use]
    pub const fn offset(&self) -> Option<u64> {
        match self.geometry {
            Some(geometry) => geometry.offset,
            None => None,
        }
    }

    /// Whether the guest cannot take a reclaim: it has not negotiated, or one
    /// is outstanding.
    #[must_use]
    pub fn asked(&self) -> bool {
        self.session
            .as_ref()
            .and_then(Session::negotiated)
            .is_none_or(|session| session.reclaim.is_some())
    }

    /// The session a reclaim goes to, unless the guest is [`asked`](Self::asked).
    pub fn askable(&mut self) -> Option<(&PeerId, &mut Negotiated)> {
        let Session { peer, phase } = self.session.as_mut()?;

        let session = phase.negotiated_mut()?;

        if session.reclaim.is_some() {
            return None;
        }

        Some((peer, session))
    }

    /// Whether a shrink is on its way to the device.
    #[must_use]
    pub fn shrinking(&self) -> bool {
        self.reconcile.is_some_and(Reconcile::is_shrink)
    }

    #[must_use]
    pub const fn requested(&self) -> Option<u64> {
        match self.reconcile {
            Some(reconcile) => Some(reconcile.target),
            None => None,
        }
    }

    pub const fn settle(&mut self, size: u64, hotplugged: u64) {
        let offset = match self.geometry {
            Some(geometry) => geometry.offset,
            None => None,
        };

        self.geometry = Some(Geometry::settled(size, hotplugged, offset));
    }

    pub const fn observe(&mut self, total_mem: u64) {
        if self.reconcile.is_some() {
            return;
        }

        if let Some(geometry) = &mut self.geometry
            && geometry.size >= total_mem
        {
            geometry.offset = Some(geometry.size - total_mem);
        }
    }

    /// Absent until the device has reported a size.
    #[must_use]
    pub fn view<'a>(&'a self, id: GuestId, spec: &'a GuestSpec) -> Option<GuestView<'a>> {
        Some(self.view_at(id, spec, self.geometry?))
    }

    #[must_use]
    pub fn view_at<'a>(
        &'a self,
        id: GuestId,
        spec: &'a GuestSpec,
        geometry: Geometry,
    ) -> GuestView<'a> {
        GuestView {
            id,
            spec,
            geometry,
            requested: self.requested(),
            session: self.session.as_ref().and_then(Session::negotiated),
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

    pub deadline: Deadline,
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
    pub fn view(&self, id: GuestId) -> Option<GuestView<'_>> {
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
    pub fn get(&self, id: GuestId) -> &Entry {
        &self.entries[id.index()]
    }

    pub fn get_mut(&mut self, id: GuestId) -> &mut Entry {
        &mut self.entries[id.index()]
    }

    #[must_use]
    pub fn state(&self, id: GuestId) -> &Guest {
        &self.get(id).state
    }

    pub fn state_mut(&mut self, id: GuestId) -> &mut Guest {
        &mut self.get_mut(id).state
    }

    #[must_use]
    pub fn session(&self, id: GuestId) -> Option<&Session> {
        self.state(id).session.as_ref()
    }

    #[must_use]
    pub fn negotiated(&self, id: GuestId) -> Option<&Negotiated> {
        self.session(id)?.negotiated()
    }

    pub fn session_mut(&mut self, id: GuestId) -> Option<&mut Session> {
        self.state_mut(id).session.as_mut()
    }

    #[must_use]
    pub fn ceiling(&self, id: GuestId) -> NonZeroU64 {
        self.get(id).spec.ceiling
    }

    #[must_use]
    pub fn name(&self, id: GuestId) -> &str {
        &self.get(id).spec.name
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (GuestId, &Entry)> {
        self.entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (GuestId::from_index(index), entry))
    }

    /// Every guest but `asking` that has reported a size.
    #[must_use]
    pub fn others(&self, asking: GuestId) -> Vec<GuestView<'_>> {
        self.iter()
            .filter(|&(id, _)| id != asking)
            .filter_map(|(id, entry)| entry.view(id))
            .collect()
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
            deadline: Deadline::after(at(30), std::time::Duration::ZERO),
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
