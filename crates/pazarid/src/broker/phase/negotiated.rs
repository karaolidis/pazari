use pazari_proto::{DenyReason, GrantReason, Status, Urgency};
use pazari_types::Timestamp;
use tracing::{debug, info, warn};

use crate::broker::{
    arbiter::{Ask, Decision},
    context::Context,
    phase::{Phase, Said, State},
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Negotiated {
    /// What the guest said its geometry was in `hello`.
    pub boot_mem: u64,
    pub block_size: u64,

    /// The last guest-initiated id answered.
    pub answered: Option<u64>,

    /// The next host-initiated id.
    pub next_reclaim: u64,

    pub reclaim: Option<Reclaim>,

    pub status: Option<Status>,

    /// When that `status` or `welcome` arrived.
    pub heard: Timestamp,

    pub stale: bool,
}

impl Negotiated {
    #[must_use]
    pub const fn new(boot_mem: u64, block_size: u64, now: Timestamp) -> Self {
        Self {
            boot_mem,
            block_size,
            answered: None,
            next_reclaim: 1,
            reclaim: None,
            status: None,
            heard: now,
            stale: false,
        }
    }

    #[must_use]
    pub const fn accepts(&self, id: u64) -> bool {
        match self.answered {
            Some(answered) => id > answered,
            None => true,
        }
    }

    pub const fn accept(&mut self, id: u64) -> bool {
        if !self.accepts(id) {
            return false;
        }

        self.answered = Some(id);

        true
    }

    pub const fn take_reclaim_id(&mut self) -> u64 {
        let id = self.next_reclaim;
        self.next_reclaim = self.next_reclaim.saturating_add(1);
        id
    }

    #[must_use]
    const fn answers_reclaim(&self, id: u64) -> bool {
        match self.reclaim {
            Some(reclaim) => reclaim.id == id,
            None => false,
        }
    }

    fn accepted(mut self, ctx: &mut Context, said: Said<'_>, id: u64) -> Option<Self> {
        if self.accept(id) {
            return Some(self);
        }

        warn!(
            guest = ctx.name(said.guest),
            peer = %said.peer,
            id,
            "a request id that does not follow the last one",
        );
        ctx.close(said.guest, said.peer);

        None
    }

    fn stray_reclaim(self, ctx: &mut Context, said: Said<'_>, id: u64) -> Phase {
        warn!(
            guest = ctx.name(said.guest),
            peer = %said.peer,
            id,
            "an answer to a reclaim the host never asked for",
        );
        ctx.close(said.guest, said.peer);

        self.stay()
    }
}

impl State for Negotiated {
    fn stay(self) -> Phase {
        Phase::Negotiated(self)
    }

    fn refuse(self, ctx: &mut Context, said: Said<'_>, _what: &'static str) -> Phase {
        warn!(
            guest = ctx.name(said.guest),
            peer = %said.peer,
            "the guest said hello twice",
        );

        ctx.close(said.guest, said.peer);

        self.stay()
    }

    fn on_grant_request(
        self,
        ctx: &mut Context,
        said: Said<'_>,
        id: u64,
        target: u64,
        reason: GrantReason,
        urgency: Urgency,
    ) -> Phase {
        let Some(next) = self.accepted(ctx, said, id) else {
            return self.stay();
        };

        let ask = match (ctx.ceiling(said.guest), ctx.size(said.guest)) {
            (Some(ceiling), Some(size)) => {
                let target = target.min(ceiling.get());

                if target > size {
                    Ok(Ask::Grow {
                        target,
                        reason,
                        urgency,
                    })
                } else {
                    Err(Decision::Deny {
                        reason: DenyReason::Ceiling,
                        retry_after: None,
                    })
                }
            }
            (None, _) => Err(ctx.unknown_deny()),
            (_, None) => Err(ctx.unsized_deny()),
        };

        ctx.request(said, id, ask);

        next.stay()
    }

    fn on_offer_release(
        self,
        ctx: &mut Context,
        said: Said<'_>,
        id: u64,
        releasable: u64,
    ) -> Phase {
        let Some(next) = self.accepted(ctx, said, id) else {
            return self.stay();
        };

        let ask = ctx
            .size(said.guest)
            .ok_or_else(|| ctx.unsized_deny())
            .map(|size| Ask::Release {
                target: size.saturating_sub(releasable),
            });

        ctx.request(said, id, ask);

        next.stay()
    }

    fn on_reclaim_ack(
        mut self,
        ctx: &mut Context,
        said: Said<'_>,
        id: u64,
        will_release: u64,
    ) -> Phase {
        if !self.answers_reclaim(id) {
            return self.stray_reclaim(ctx, said, id);
        }

        if let Some(reclaim) = self.reclaim.as_mut() {
            reclaim.committed = Some(will_release);
        }

        info!(
            guest = ctx.name(said.guest),
            id, will_release, "the guest took on a release"
        );

        self.stay()
    }

    fn on_reclaim_nak(mut self, ctx: &mut Context, said: Said<'_>, id: u64, reason: &str) -> Phase {
        if !self.answers_reclaim(id) {
            return self.stray_reclaim(ctx, said, id);
        }

        if let Some(reclaim) = self.reclaim.as_mut() {
            reclaim.refused = true;
        }

        info!(
            guest = ctx.name(said.guest),
            id, reason, "the guest refused a release"
        );

        self.stay()
    }

    fn on_status(mut self, ctx: &mut Context, said: Said<'_>, status: Status) -> Phase {
        let returned = self.stale;

        self.stale = false;
        self.heard = said.now;
        self.status = Some(status);

        if returned {
            info!(
                guest = ctx.name(said.guest),
                "the guest is sending status again"
            );
        }

        self.stay()
    }

    fn on_bye(self, ctx: &mut Context, said: Said<'_>) -> Phase {
        debug!(guest = ctx.name(said.guest), peer = %said.peer, "the guest said goodbye");

        ctx.close(said.guest, said.peer);

        self.stay()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reclaim {
    pub id: u64,

    pub desired: u64,

    pub deadline: Timestamp,

    pub committed: Option<u64>,

    pub refused: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::testing::{BLOCK, BOOT, at};

    fn negotiated() -> Negotiated {
        Negotiated::new(BOOT, BLOCK, at(0))
    }

    #[test]
    fn guest_ids_must_climb() {
        let mut session = negotiated();

        assert!(session.accepts(0));
        assert!(session.accepts(9));

        assert!(session.accept(4));
        assert_eq!(session.answered, Some(4));

        assert!(!session.accepts(3));
        assert!(!session.accept(4));
        assert!(session.accepts(5));
        assert_eq!(session.answered, Some(4));

        assert!(session.accept(5));
        assert_eq!(session.answered, Some(5));
    }

    #[test]
    fn host_ids_start_at_one_and_climb() {
        let mut session = negotiated();

        assert_eq!(session.take_reclaim_id(), 1);
        assert_eq!(session.take_reclaim_id(), 2);
        assert_eq!(session.take_reclaim_id(), 3);
    }
}
