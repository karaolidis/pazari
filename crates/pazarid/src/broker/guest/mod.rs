use pazari_types::Timestamp;

use crate::broker::session::Session;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Guest {
    pub size: u64,

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
