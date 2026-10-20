use std::num::NonZeroU64;

use pazari_proto::{DenyReason, ReclaimId, RequestId, Status};
use pazari_types::{Deadline, Timestamp};
use tracing::{debug, info, warn};

use crate::broker::{
    arbiter::{Ask, Decision},
    phase::{Phase, State, Step},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reclaim {
    pub id: ReclaimId,

    pub desired: u64,

    pub deadline: Deadline,

    pub committed: Option<u64>,

    pub refused: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Negotiated {
    /// What the guest said its geometry was in `hello`.
    pub block_size: NonZeroU64,

    /// The last guest-initiated id answered.
    pub answered: Option<RequestId>,

    /// The next host-initiated id.
    pub next_reclaim: ReclaimId,

    pub reclaim: Option<Reclaim>,

    pub status: Option<Status>,

    /// When that `status` or `welcome` arrived.
    pub heard: Timestamp,

    pub stale: bool,
}

impl Negotiated {
    #[must_use]
    pub const fn new(block_size: NonZeroU64, now: Timestamp) -> Self {
        Self {
            block_size,
            answered: None,
            next_reclaim: ReclaimId(1),
            reclaim: None,
            status: None,
            heard: now,
            stale: false,
        }
    }

    #[must_use]
    pub const fn accepts(&self, id: RequestId) -> bool {
        match self.answered {
            Some(answered) => id.get() > answered.get(),
            None => true,
        }
    }

    pub const fn accept(&mut self, id: RequestId) -> bool {
        if !self.accepts(id) {
            return false;
        }

        self.answered = Some(id);

        true
    }

    pub const fn take_reclaim_id(&mut self) -> ReclaimId {
        let id = self.next_reclaim;
        self.next_reclaim = id.next();
        id
    }

    #[must_use]
    const fn answers_reclaim(&self, id: ReclaimId) -> bool {
        match self.reclaim {
            Some(reclaim) => reclaim.id.get() == id.get(),
            None => false,
        }
    }

    fn accepted(mut self, step: &mut Step<'_>, id: RequestId) -> Option<Self> {
        if self.accept(id) {
            return Some(self);
        }

        warn!(
            guest = step.name(),
            peer = %step.peer,
            %id,
            "out-of-sequence request id",
        );
        step.close();

        None
    }

    #[must_use]
    const fn blocks(&self, size: u64, wanted: u64, ceiling: u64) -> u64 {
        let blocks = wanted.saturating_sub(size).div_ceil(self.block_size.get());

        let target = size.saturating_add(blocks.saturating_mul(self.block_size.get()));

        if target < ceiling { target } else { ceiling }
    }

    fn stray_reclaim(self, step: &mut Step<'_>, id: ReclaimId) -> Phase {
        warn!(
            guest = step.name(),
            peer = %step.peer,
            %id,
            "reply to a reclaim that was never requested",
        );
        step.close();

        self.stay()
    }

    fn request(step: &mut Step<'_>, id: RequestId, ask: Result<Ask, Decision>) {
        let ask = match ask {
            Ok(ask) => ask,
            Err(decision) => return step.answer(id, decision),
        };

        let decision = step.world.arbitrate(step.now, step.guest, ask);

        let Decision::Grant { target } = decision else {
            return step.answer(id, decision);
        };

        let Some(ceiling) = step.ceiling() else {
            return step.answer(id, Decision::UNKNOWN);
        };

        let target = target.min(ceiling.get());
        let granted = target.saturating_sub(step.world.offset(step.guest));

        step.answer(id, Decision::Grant { target: granted });

        if matches!(ask, Ask::Grow { .. }) {
            step.world.note_granted(step.guest, step.now);
        }

        step.world.actuate(step.now, step.guest, target, step.fx);
    }
}

impl State for Negotiated {
    fn stay(self) -> Phase {
        Phase::Negotiated(self)
    }

    fn refuse(self, step: &mut Step<'_>, _what: &'static str) -> Phase {
        warn!(guest = step.name(), peer = %step.peer, "duplicate hello");

        step.close();

        self.stay()
    }

    fn on_grant_request(self, step: &mut Step<'_>, id: RequestId, target: u64) -> Phase {
        let Some(next) = self.accepted(step, id) else {
            return self.stay();
        };

        debug!(
            guest = step.name(),
            %id,
            target,
            size = ?step.size(),
            "a guest asked to grow",
        );

        let ask = match (step.ceiling(), step.size()) {
            (Some(ceiling), Some(size)) => {
                let ceiling = ceiling.get();
                let wanted = target
                    .saturating_add(step.world.offset(step.guest))
                    .min(ceiling);

                if wanted > size {
                    Ok(Ask::Grow {
                        target: self.blocks(size, wanted, ceiling),
                    })
                } else {
                    Err(Decision::Deny {
                        reason: DenyReason::Ceiling,
                        retry_after: None,
                    })
                }
            }
            (None, _) => Err(Decision::UNKNOWN),
            (_, None) => Err(step.world.unsized_deny()),
        };

        Self::request(step, id, ask);

        next.stay()
    }

    fn on_offer_release(self, step: &mut Step<'_>, id: RequestId, releasable: u64) -> Phase {
        let Some(next) = self.accepted(step, id) else {
            return self.stay();
        };

        debug!(
            guest = step.name(),
            %id,
            releasable,
            size = ?step.size(),
            "guest offered memory back",
        );

        let base = step
            .size()
            .unwrap_or(0)
            .saturating_sub(step.world.hotplugged(step.guest).unwrap_or(0));

        let ask = step
            .size()
            .ok_or_else(|| step.world.unsized_deny())
            .map(|size| Ask::Release {
                target: size.saturating_sub(releasable).max(base),
            });

        Self::request(step, id, ask);

        next.stay()
    }

    fn on_reclaim_ack(mut self, step: &mut Step<'_>, id: ReclaimId, will_release: u64) -> Phase {
        if !self.answers_reclaim(id) {
            return self.stray_reclaim(step, id);
        }

        if let Some(reclaim) = self.reclaim.as_mut() {
            reclaim.committed = Some(will_release);
        }

        info!(
            guest = step.name(),
            %id, will_release, "guest accepted the release"
        );

        self.stay()
    }

    fn on_reclaim_nak(mut self, step: &mut Step<'_>, id: ReclaimId, reason: &str) -> Phase {
        if !self.answers_reclaim(id) {
            return self.stray_reclaim(step, id);
        }

        if let Some(reclaim) = self.reclaim.as_mut() {
            reclaim.refused = true;
        }

        info!(guest = step.name(), %id, reason, "guest refused the release");

        self.stay()
    }

    fn on_status(mut self, step: &mut Step<'_>, status: Status) -> Phase {
        let returned = self.stale;

        self.stale = false;
        self.heard = step.now;
        self.status = Some(status);

        step.world.observe_total_mem(step.guest, status.total_mem);
        step.world.tell_geometry(step.guest, step.peer, step.fx);

        if returned {
            info!(guest = step.name(), "guest resumed sending status");
        }

        self.stay()
    }

    fn on_bye(self, step: &mut Step<'_>) -> Phase {
        debug!(guest = step.name(), peer = %step.peer, "guest sent goodbye");

        step.close();

        self.stay()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::testing::{BLOCK, at, bytes};

    fn negotiated() -> Negotiated {
        Negotiated::new(bytes(BLOCK), at(0))
    }

    #[test]
    fn guest_ids_must_climb() {
        let mut session = negotiated();

        assert!(session.accepts(RequestId(0)));
        assert!(session.accepts(RequestId(9)));

        assert!(session.accept(RequestId(4)));
        assert_eq!(session.answered, Some(RequestId(4)));

        assert!(!session.accepts(RequestId(3)));
        assert!(!session.accept(RequestId(4)));
        assert!(session.accepts(RequestId(5)));
        assert_eq!(session.answered, Some(RequestId(4)));

        assert!(session.accept(RequestId(5)));
        assert_eq!(session.answered, Some(RequestId(5)));
    }

    #[test]
    fn host_ids_start_at_one_and_climb() {
        let mut session = negotiated();

        assert_eq!(session.take_reclaim_id(), ReclaimId(1));
        assert_eq!(session.take_reclaim_id(), ReclaimId(2));
        assert_eq!(session.take_reclaim_id(), ReclaimId(3));
    }

    #[test]
    fn a_grow_lands_on_a_whole_number_of_the_guests_blocks() {
        let session = negotiated();

        assert_eq!(session.blocks(2048, 2049, u64::MAX), 2048 + BLOCK);
        assert_eq!(session.blocks(2048, 2048 + BLOCK, u64::MAX), 2048 + BLOCK);
        assert_eq!(session.blocks(2048, 4096, 2048 + BLOCK), 2048 + BLOCK);
    }
}
