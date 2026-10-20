use std::num::NonZeroU64;

use pazari_proto::{DenyReason, ReclaimId, RequestId, Status};
use pazari_types::{Deadline, Timestamp};
use tracing::{debug, info, warn};

use crate::broker::{
    arbiter::{Ask, Decision},
    guest::Geometry,
    phase::{Phase, State, Step},
};

/// How a guest has answered a reclaim request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    Pending,

    /// The guest promised to release this much.
    Committed(u64),

    /// The guest is not asked again until the deadline.
    Refused,
}

/// Whether a guest is keeping to its `status` interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reporting {
    Fresh,

    /// It has missed more intervals than `status.stale_after`.
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reclaim {
    pub(crate) id: ReclaimId,

    pub(crate) desired: u64,

    pub(crate) deadline: Deadline,

    pub(crate) reply: Reply,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Negotiated {
    /// What the guest said its geometry was in `hello`.
    pub(crate) block_size: NonZeroU64,

    /// The last guest-initiated id answered.
    pub(crate) answered: Option<RequestId>,

    /// The next host-initiated id.
    pub(crate) next_reclaim: ReclaimId,

    pub(crate) reclaim: Option<Reclaim>,

    pub(crate) status: Option<Status>,

    /// When that `status` or `welcome` arrived.
    pub(crate) heard: Timestamp,

    pub(crate) reporting: Reporting,
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
            reporting: Reporting::Fresh,
        }
    }

    #[must_use]
    pub const fn block_size(&self) -> NonZeroU64 {
        self.block_size
    }

    #[must_use]
    pub const fn status(&self) -> Option<Status> {
        self.status
    }

    #[must_use]
    pub const fn reporting(&self) -> Reporting {
        self.reporting
    }

    #[must_use]
    pub const fn reclaim(&self) -> Option<Reclaim> {
        self.reclaim
    }

    #[must_use]
    pub fn accepts(&self, id: RequestId) -> bool {
        self.answered.is_none_or(|answered| id > answered)
    }

    pub fn accept(&mut self, id: RequestId) -> bool {
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

    /// Takes the first reply to the outstanding reclaim; any other is stray.
    fn answer_reclaim(&mut self, id: ReclaimId, reply: Reply) -> bool {
        match &mut self.reclaim {
            Some(reclaim) if reclaim.id == id && matches!(reclaim.reply, Reply::Pending) => {
                reclaim.reply = reply;

                true
            }
            _ => false,
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

    /// The most of `bytes` that is a whole number of the guest's blocks.
    #[must_use]
    const fn whole_blocks(&self, bytes: u64) -> u64 {
        bytes - bytes % self.block_size.get()
    }

    /// The fewest whole blocks of the guest's that hold `bytes`.
    #[must_use]
    const fn blocks_holding(&self, bytes: u64) -> u64 {
        bytes
            .div_ceil(self.block_size.get())
            .saturating_mul(self.block_size.get())
    }

    /// What a grow to `target`, as the guest reads it, asks of the device.
    ///
    /// An `Err` is answered without the arbiter.
    fn grow(&self, step: &Step<'_>, target: u64) -> Result<(Geometry, Ask), Decision> {
        let ceiling = step.ceiling();
        let geometry = step.geometry().ok_or_else(|| step.world.unsized_deny())?;

        // Where the device is heading, which a resize in flight moves off its size.
        let heading = step.requested().unwrap_or(geometry.size);
        let wanted = geometry.device(target);

        if wanted <= heading {
            return Err(Decision::Grant { target });
        }

        let room = self.whole_blocks(ceiling.get().saturating_sub(heading));

        if room == 0 {
            return Err(Decision::Deny {
                reason: DenyReason::Ceiling,
                retry_after: None,
            });
        }

        let grow = self.blocks_holding(wanted - heading).min(room);

        Ok((
            geometry,
            Ask::Grow {
                target: heading + grow,
            },
        ))
    }

    /// What an offer of `releasable` bytes asks of the device.
    ///
    /// An `Err` is answered without the arbiter.
    fn release(&self, step: &Step<'_>, releasable: u64) -> Result<(Geometry, Ask), Decision> {
        let geometry = step.geometry().ok_or_else(|| step.world.unsized_deny())?;

        let target = geometry
            .size
            .saturating_sub(self.whole_blocks(releasable))
            .max(geometry.boot());

        Ok((geometry, Ask::Release { target }))
    }

    /// Settles a reclaim the guest promised once it offers that much back.
    fn fulfil(&mut self, step: &Step<'_>, releasable: u64) {
        let Some(reclaim) = self.reclaim else {
            return;
        };

        let Reply::Committed(committed) = reclaim.reply else {
            return;
        };

        if releasable < committed {
            return;
        }

        self.reclaim = None;

        info!(
            guest = step.name(),
            id = %reclaim.id,
            committed,
            releasable,
            "guest released what it promised",
        );
    }

    fn stray_reclaim(self, step: &mut Step<'_>, id: ReclaimId) -> Phase {
        warn!(
            guest = step.name(),
            peer = %step.peer,
            %id,
            "reply to a reclaim that is not awaiting one",
        );
        step.close();

        self.stay()
    }

    fn request(step: &mut Step<'_>, id: RequestId, asked: Result<(Geometry, Ask), Decision>) {
        let (geometry, ask) = match asked {
            Ok(asked) => asked,
            Err(decision) => return step.answer(id, decision),
        };

        let decision = step.world.arbitrate(step.now, step.guest, geometry, ask);

        let Decision::Grant { target } = decision else {
            return step.answer(id, decision);
        };

        let target = target.min(step.ceiling().get());

        step.answer(
            id,
            Decision::Grant {
                target: geometry.seen(target),
            },
        );

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

    fn refuse(self, step: &mut Step<'_>, what: &'static str) -> Phase {
        warn!(
            guest = step.name(),
            peer = %step.peer,
            said = what,
            "guest sent a message out of turn",
        );

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
            geometry = ?step.geometry(),
            "a guest asked to grow",
        );

        let asked = next.grow(step, target);

        Self::request(step, id, asked);

        next.stay()
    }

    fn on_offer_release(self, step: &mut Step<'_>, id: RequestId, releasable: u64) -> Phase {
        let Some(mut next) = self.accepted(step, id) else {
            return self.stay();
        };

        debug!(
            guest = step.name(),
            %id,
            releasable,
            geometry = ?step.geometry(),
            "guest offered memory back",
        );

        next.fulfil(step, releasable);

        let asked = next.release(step, releasable);

        Self::request(step, id, asked);

        next.stay()
    }

    fn on_reclaim_ack(mut self, step: &mut Step<'_>, id: ReclaimId, will_release: u64) -> Phase {
        if !self.answer_reclaim(id, Reply::Committed(will_release)) {
            return self.stray_reclaim(step, id);
        }

        info!(
            guest = step.name(),
            %id, will_release, "guest accepted the release"
        );

        self.stay()
    }

    fn on_reclaim_nak(mut self, step: &mut Step<'_>, id: ReclaimId, reason: &str) -> Phase {
        if !self.answer_reclaim(id, Reply::Refused) {
            return self.stray_reclaim(step, id);
        }

        info!(guest = step.name(), %id, reason, "guest refused the release");

        self.stay()
    }

    fn on_status(mut self, step: &mut Step<'_>, status: Status) -> Phase {
        let returned = self.reporting == Reporting::Stale;

        self.reporting = Reporting::Fresh;
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
    fn sizes_are_taken_in_whole_blocks_of_the_guest_s() {
        let session = negotiated();

        assert_eq!(session.blocks_holding(1), BLOCK);
        assert_eq!(session.blocks_holding(BLOCK), BLOCK);
        assert_eq!(session.blocks_holding(BLOCK + 1), 2 * BLOCK);

        assert_eq!(session.whole_blocks(BLOCK - 1), 0);
        assert_eq!(session.whole_blocks(BLOCK), BLOCK);
        assert_eq!(session.whole_blocks(2 * BLOCK - 1), BLOCK);
    }
}
