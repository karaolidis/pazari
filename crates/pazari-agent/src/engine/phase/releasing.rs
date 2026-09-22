use std::time::Duration;

use pazari_proto::{DenyReason, GuestMessage};
use pazari_types::Timestamp;

use crate::engine::{
    action::Action,
    context::Context,
    limits::Band,
    phase::{Phase, State, growing::Growing, steady::Steady},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Releasing {
    Reclaiming { bytes: u64 },
    Offering { id: u64, target: u64 },
    Settling { target: u64, expires: Timestamp },
}

impl Releasing {
    pub fn begin(ctx: &mut Context, bytes: u64) -> Phase {
        let swappiness = ctx.config.reclaim_swappiness;

        ctx.emit(Action::StartReclaim { bytes, swappiness });

        Phase::Releasing(Self::Reclaiming { bytes })
    }

    pub fn finish(ctx: &mut Context, now: Timestamp) -> Phase {
        ctx.idle.reset();

        if ctx.limits.band() == Band::Open {
            ctx.apply_limits(Band::Open);

            return Growing::start(ctx, now, None);
        }

        ctx.apply_limits(Band::Steady);

        Phase::Steady(Steady)
    }

    pub const fn pending_request(self) -> Option<u64> {
        match self {
            Self::Offering { id, .. } => Some(id),
            _ => None,
        }
    }

    const fn answers(self, id: u64) -> bool {
        matches!(self.pending_request(), Some(pending) if pending == id)
    }
}

impl State for Releasing {
    fn stay(self) -> Phase {
        Phase::Releasing(self)
    }

    fn busy(self) -> &'static str {
        "a release is already in progress"
    }

    fn advance(self, ctx: &mut Context, now: Timestamp) -> Phase {
        match self {
            Self::Settling { expires, .. } if now >= expires => Self::finish(ctx, now),
            _ => self.stay(),
        }
    }

    fn brake(self, ctx: &mut Context, now: Timestamp) -> Phase {
        match self {
            Self::Reclaiming { .. } => Growing::start(ctx, now, None),
            _ => self.stay(),
        }
    }

    fn on_plugged(self, ctx: &mut Context, now: Timestamp, changed: bool) -> Phase {
        match self {
            Self::Settling { target, .. } if ctx.limits.plugged() <= target => {
                Self::finish(ctx, now)
            }
            _ => {
                if changed {
                    ctx.reapply_limits();
                }

                self.stay()
            }
        }
    }

    fn on_grant(self, ctx: &mut Context, now: Timestamp, id: u64, target: u64) -> Phase {
        if !self.answers(id) {
            return self.stay();
        }

        let expires = now.saturating_add(ctx.config.settle_grace.get());

        Self::Settling { target, expires }.on_plugged(ctx, now, false)
    }

    fn on_deny(
        self,
        ctx: &mut Context,
        now: Timestamp,
        id: u64,
        _reason: DenyReason,
        _retry_after: Option<Duration>,
    ) -> Phase {
        if !self.answers(id) {
            return self.stay();
        }

        Self::finish(ctx, now)
    }

    fn on_reclaim_finished(self, ctx: &mut Context, now: Timestamp, reclaimed: u64) -> Phase {
        let Self::Reclaiming { bytes } = self else {
            return self.stay();
        };

        if !ctx.connected {
            return Self::finish(ctx, now);
        }

        let headroom = ctx.round_down(ctx.last.available.saturating_add(reclaimed));
        let releasable = ctx.release_budget(bytes.min(headroom));

        if releasable == 0 {
            return Self::finish(ctx, now);
        }

        let id = ctx.take_id();
        let current = ctx.limits.plugged();

        ctx.send(GuestMessage::OfferRelease {
            id,
            current,
            releasable,
        });

        Phase::Releasing(Self::Offering {
            id,
            target: current.saturating_sub(releasable),
        })
    }

    fn on_connected(self) -> Phase {
        Phase::Steady(Steady)
    }

    fn on_disconnected(self, ctx: &mut Context, now: Timestamp) -> Phase {
        Self::finish(ctx, now)
    }
}

#[cfg(test)]
mod tests {
    use pazari_proto::{DenyReason, GuestMessage, HostMessage};

    use crate::engine::{Action, Event, Mode, testing::*};

    #[test]
    fn a_reclaim_becomes_an_offer_and_settles_downward() {
        let mut engine = started();
        grown(&mut engine);

        for second in 140..=143 {
            tick(&mut engine, at(second), snap(2560 * MIB, CALM, 1));
        }

        assert_eq!(
            engine.handle(Event::ReclaimFinished {
                now: at(144),
                reclaimed: 256 * MIB,
            }),
            vec![Action::Send(GuestMessage::OfferRelease {
                id: 2,
                current: 2560 * MIB,
                releasable: 256 * MIB,
            })]
        );

        assert_eq!(
            host(
                &mut engine,
                at(145),
                HostMessage::Grant {
                    id: 2,
                    target: 2304 * MIB,
                },
            ),
            vec![]
        );

        assert_eq!(
            engine.handle(Event::PluggedChanged {
                now: at(146),
                plugged: 2304 * MIB,
            }),
            vec![Action::SetHigh(1792 * MIB), Action::SetMax(2048 * MIB)]
        );
        assert_eq!(engine.mode(), Mode::Steady);
    }

    #[test]
    fn a_denied_offer_returns_to_steady() {
        let mut engine = started();
        grown(&mut engine);

        for second in 140..=143 {
            tick(&mut engine, at(second), snap(2560 * MIB, CALM, 1));
        }
        engine.handle(Event::ReclaimFinished {
            now: at(144),
            reclaimed: 256 * MIB,
        });

        assert_eq!(
            host(
                &mut engine,
                at(145),
                HostMessage::Deny {
                    id: 2,
                    reason: DenyReason::Policy,
                    retry_after_ms: None,
                },
            ),
            vec![]
        );
        assert_eq!(engine.mode(), Mode::Steady);
        assert_eq!(engine.plugged(), 2560 * MIB);
    }
}
