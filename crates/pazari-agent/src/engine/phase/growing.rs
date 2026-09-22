use std::time::Duration;

use pazari_proto::{DenyReason, GrantReason, GuestMessage, Urgency};
use pazari_types::Timestamp;

use crate::engine::{
    context::Context,
    limits::Band,
    phase::{Phase, State, exhausted::Exhausted, steady::Steady},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Growth {
    Blocked,
    Requesting { id: u64, target: u64 },
    Backoff { retry_at: Timestamp },
    Settling { target: u64, expires: Timestamp },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Growing {
    exhausted_since: Option<Timestamp>,
    state: Growth,
}

impl Growing {
    pub fn start(ctx: &mut Context, now: Timestamp, exhausted_since: Option<Timestamp>) -> Phase {
        ctx.apply_limits(Band::Open);

        if !ctx.connected {
            return Self::at(exhausted_since, Growth::Blocked);
        }

        let Some(target) = ctx.grow_target() else {
            return Exhausted::enter(ctx, now, exhausted_since);
        };

        let id = ctx.take_id();
        let current = ctx.limits.plugged();

        ctx.send(GuestMessage::GrantRequest {
            id,
            current,
            target,
            reason: GrantReason::Throttle,
            urgency: Urgency::High,
        });

        Self::at(exhausted_since, Growth::Requesting { id, target })
    }

    const fn at(exhausted_since: Option<Timestamp>, state: Growth) -> Phase {
        Phase::Growing(Self {
            exhausted_since,
            state,
        })
    }

    pub const fn pending_request(self) -> Option<u64> {
        match self.state {
            Growth::Requesting { id, .. } => Some(id),
            _ => None,
        }
    }

    const fn answers(self, id: u64) -> bool {
        matches!(self.pending_request(), Some(pending) if pending == id)
    }

    const fn blocked(self) -> Phase {
        match self.state {
            Growth::Requesting { .. } | Growth::Backoff { .. } => {
                Self::at(self.exhausted_since, Growth::Blocked)
            }
            _ => Phase::Growing(self),
        }
    }
}

impl State for Growing {
    fn stay(self) -> Phase {
        Phase::Growing(self)
    }

    fn busy(self) -> &'static str {
        "a grant request is outstanding"
    }

    fn advance(self, ctx: &mut Context, now: Timestamp) -> Phase {
        match self.state {
            Growth::Backoff { retry_at } if now >= retry_at => {
                Self::start(ctx, now, self.exhausted_since)
            }
            Growth::Settling { expires, .. } if now >= expires => Steady::settle(ctx, now),
            _ => self.stay(),
        }
    }

    fn brake(self, _ctx: &mut Context, _now: Timestamp) -> Phase {
        self.stay()
    }

    fn on_plugged(self, ctx: &mut Context, now: Timestamp, changed: bool) -> Phase {
        match self.state {
            Growth::Settling { target, .. } if ctx.limits.plugged() >= target => {
                Steady::settle(ctx, now)
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
        let settling = Self::at(self.exhausted_since, Growth::Settling { target, expires });

        settling.on_plugged(ctx, now, false)
    }

    fn on_deny(
        self,
        ctx: &mut Context,
        now: Timestamp,
        id: u64,
        reason: DenyReason,
        retry_after: Option<Duration>,
    ) -> Phase {
        if !self.answers(id) {
            return self.stay();
        }

        if reason == DenyReason::Ceiling || ctx.grow_target().is_none() {
            return Exhausted::enter(ctx, now, self.exhausted_since);
        }

        let backoff = retry_after.unwrap_or_else(|| ctx.config.deny_backoff.get());

        Self::at(
            self.exhausted_since,
            Growth::Backoff {
                retry_at: now.saturating_add(backoff),
            },
        )
    }

    fn on_connected(self) -> Phase {
        self.blocked()
    }

    fn on_disconnected(self, _ctx: &mut Context, _now: Timestamp) -> Phase {
        self.blocked()
    }

    fn resume(self, ctx: &mut Context, now: Timestamp) -> Phase {
        match self.state {
            Growth::Settling { .. } => self.stay(),
            _ => Self::start(ctx, now, self.exhausted_since),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pazari_proto::{DenyReason, GuestMessage, HostMessage};

    use crate::engine::{Action, Event, Mode, testing::*};

    #[test]
    fn the_request_target_covers_the_deficit_and_rounds_up_to_a_block() {
        let mut engine = started();

        tick(&mut engine, at(60), snap(BOOT, 2200 * MIB, 0));

        assert_eq!(
            wake(&mut engine, at(61), 1),
            vec![Action::SetHigh(OPEN0), request(1, BOOT, 2816 * MIB)]
        );
    }

    #[test]
    fn the_request_target_is_clamped_and_aligned_under_the_ceiling() {
        let mut engine = titrated(config(), 2200 * MIB);

        assert_eq!(
            wake(&mut engine, at(100), 1),
            vec![Action::SetHigh(OPEN0), request(1, BOOT, 2176 * MIB)]
        );
    }

    #[test]
    fn a_grant_wait_keeps_the_guard_engaged() {
        let mut engine = started();

        wake(&mut engine, at(100), 1);

        assert_eq!(engine.pending_request(), Some(1));
        assert_eq!(engine.high(), OPEN0);
        assert!(engine.high() < engine.max());
    }

    #[test]
    fn only_one_grant_request_is_in_flight() {
        let mut engine = started();

        wake(&mut engine, at(100), 1);

        assert_eq!(wake(&mut engine, at(101), 2), vec![]);
        assert_eq!(wake(&mut engine, at(102), 3), vec![]);
        assert_eq!(engine.pending_request(), Some(1));
    }

    #[test]
    fn a_grant_settles_into_recomputed_limits() {
        let mut engine = started();

        wake(&mut engine, at(100), 1);

        assert_eq!(
            host(
                &mut engine,
                at(101),
                HostMessage::Grant {
                    id: 1,
                    target: 2560 * MIB,
                },
            ),
            vec![]
        );
        assert_eq!(engine.mode(), Mode::AwaitingGrant);

        assert_eq!(
            engine.handle(Event::PluggedChanged {
                now: at(102),
                plugged: 2560 * MIB,
            }),
            vec![Action::SetMax(2304 * MIB), Action::SetHigh(2048 * MIB)]
        );
        assert_eq!(engine.mode(), Mode::Steady);
        assert_eq!(engine.plugged(), 2560 * MIB);
    }

    #[test]
    fn a_partial_grant_settles_at_what_was_granted() {
        let mut engine = started();

        wake(&mut engine, at(100), 1);
        host(
            &mut engine,
            at(101),
            HostMessage::Grant {
                id: 1,
                target: 2304 * MIB,
            },
        );

        assert_eq!(
            engine.handle(Event::PluggedChanged {
                now: at(102),
                plugged: 2304 * MIB,
            }),
            vec![Action::SetMax(2048 * MIB), Action::SetHigh(1792 * MIB)]
        );
        assert_eq!(engine.mode(), Mode::Steady);
        assert_eq!(engine.max(), 2048 * MIB);
        assert_eq!(engine.high(), 1792 * MIB);
    }

    #[test]
    fn a_partial_plug_settles_from_actual_plugged_after_the_grace() {
        let mut engine = started();

        wake(&mut engine, at(100), 1);
        host(
            &mut engine,
            at(101),
            HostMessage::Grant {
                id: 1,
                target: 2560 * MIB,
            },
        );

        assert_eq!(
            engine.handle(Event::PluggedChanged {
                now: at(102),
                plugged: 2304 * MIB,
            }),
            vec![Action::SetMax(2048 * MIB), Action::SetHigh(1984 * MIB)]
        );
        assert_eq!(engine.mode(), Mode::AwaitingGrant);

        assert_eq!(
            tick(&mut engine, at(110), snap(2304 * MIB, BUSY, 1)),
            vec![]
        );
        assert_eq!(
            tick(&mut engine, at(111), snap(2304 * MIB, BUSY, 1)),
            vec![Action::SetHigh(1792 * MIB)]
        );
        assert_eq!(engine.mode(), Mode::Steady);
    }

    #[test]
    fn a_deny_is_not_retried_before_retry_after() {
        let mut engine = started();

        wake(&mut engine, at(100), 1);

        assert_eq!(
            host(
                &mut engine,
                at(101),
                HostMessage::Deny {
                    id: 1,
                    reason: DenyReason::HostPressure,
                    retry_after_ms: Some(Duration::from_secs(5)),
                },
            ),
            vec![]
        );

        assert_eq!(tick(&mut engine, at(105), snap(BOOT, BUSY, 1)), vec![]);
        assert_eq!(
            tick(&mut engine, at(106), snap(BOOT, BUSY, 1)),
            vec![request(2, BOOT, 2560 * MIB)]
        );
        assert_eq!(engine.pending_request(), Some(2));
    }

    #[test]
    fn a_deny_without_advice_retries_after_the_configured_backoff() {
        let mut engine = started();

        wake(&mut engine, at(100), 1);
        host(
            &mut engine,
            at(100),
            HostMessage::Deny {
                id: 1,
                reason: DenyReason::RateLimited,
                retry_after_ms: None,
            },
        );

        assert_eq!(tick(&mut engine, at(104), snap(BOOT, BUSY, 1)), vec![]);
        assert_eq!(
            tick(&mut engine, at(105), snap(BOOT, BUSY, 1)),
            vec![request(2, BOOT, 2560 * MIB)]
        );
    }

    #[test]
    fn a_host_reclaim_request_is_naked_while_growing() {
        let mut engine = started();

        wake(&mut engine, at(100), 1);

        let actions = host(
            &mut engine,
            at(101),
            HostMessage::ReclaimRequest {
                id: 11,
                desired: 128 * MIB,
                deadline_ms: Duration::from_secs(30),
            },
        );

        assert!(matches!(
            actions.as_slice(),
            [Action::Send(GuestMessage::ReclaimNak { id: 11, .. })]
        ));
        assert_eq!(engine.pending_request(), Some(1));
    }

    #[test]
    fn request_ids_increase_within_a_connection() {
        let mut engine = started();

        wake(&mut engine, at(100), 1);

        for round in 0..3 {
            let id = engine.pending_request().expect("a request is in flight");
            assert_eq!(id, round + 1);

            host(
                &mut engine,
                at(100 + round * 10),
                HostMessage::Deny {
                    id,
                    reason: DenyReason::HostPressure,
                    retry_after_ms: Some(Duration::from_secs(5)),
                },
            );

            tick(&mut engine, at(105 + round * 10), snap(BOOT, BUSY, 1));
        }

        assert_eq!(engine.pending_request(), Some(4));
    }

    #[test]
    fn an_answer_to_an_unknown_id_is_ignored() {
        let mut engine = started();

        wake(&mut engine, at(100), 1);

        assert_eq!(
            host(
                &mut engine,
                at(101),
                HostMessage::Grant {
                    id: 99,
                    target: 8192 * MIB,
                },
            ),
            vec![]
        );
        assert_eq!(engine.pending_request(), Some(1));
        assert_eq!(engine.mode(), Mode::AwaitingGrant);
    }
}
