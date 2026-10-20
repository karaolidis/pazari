use std::time::Duration;

use pazari_proto::{DenyReason, GuestMessage};
use pazari_types::{Deadline, Timestamp};
use tracing::debug;

use crate::engine::{
    counters::Pressure,
    grow::Grow,
    limits::Brake,
    phase::{Phase, State, Step, exhausted::Exhausted, steady::Steady},
    world::RequestId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Blocked,
    Requesting { id: RequestId, target: u64 },
    Backoff { retry_at: Deadline },
    Settling { target: u64, expires: Deadline },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Growing {
    exhausted_since: Option<Timestamp>,
    stage: Stage,
}

impl Growing {
    pub fn braked(step: &mut Step<'_>) -> Phase {
        Self::braked_at(step, step.now)
    }

    /// Answers a brake the workload hit at `at`.
    pub fn braked_at(step: &mut Step<'_>, at: Timestamp) -> Phase {
        step.world.grow.braked(at);

        Self::start(step, None)
    }

    pub fn start(step: &mut Step<'_>, exhausted_since: Option<Timestamp>) -> Phase {
        step.brake(Brake::Engaged);

        if !step.world.connected() {
            debug!("cannot request memory: no host session");

            return Self::at(exhausted_since, Stage::Blocked);
        }

        let Some(target) = step.world.grow_target() else {
            return Exhausted::enter(step, exhausted_since);
        };

        let id = step.world.take_id();
        let current = step.limits().total_mem();

        debug!(
            id = id.get(),
            current,
            target,
            grow = ?step.world.grow,
            usage = step.guest().usage(),
            ceiling = ?step.limits().ceiling(),
            "requesting more memory from the host",
        );

        step.send(GuestMessage::GrantRequest {
            id: id.get(),
            current,
            target,
        });

        Self::at(exhausted_since, Stage::Requesting { id, target })
    }

    const fn at(exhausted_since: Option<Timestamp>, stage: Stage) -> Phase {
        Phase::Growing(Self {
            exhausted_since,
            stage,
        })
    }

    pub const fn pending_request(self) -> Option<u64> {
        match self.stage {
            Stage::Requesting { id, .. } => Some(id.get()),
            _ => None,
        }
    }

    const fn answers(self, id: u64) -> bool {
        matches!(self.pending_request(), Some(pending) if pending == id)
    }

    const fn blocked(self) -> Phase {
        match self.stage {
            Stage::Requesting { .. } | Stage::Backoff { .. } => {
                Self::at(self.exhausted_since, Stage::Blocked)
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

    fn brake(self, _step: &mut Step<'_>) -> Phase {
        self.stay()
    }

    fn resume(self, step: &mut Step<'_>) -> Phase {
        match self.stage {
            Stage::Settling { .. } => self.stay(),
            _ => Self::start(step, self.exhausted_since),
        }
    }

    fn advance(self, step: &mut Step<'_>, _pressure: Pressure) -> Phase {
        match self.stage {
            Stage::Backoff { retry_at } if retry_at.reached(step.now) => {
                Self::start(step, self.exhausted_since)
            }
            Stage::Settling { expires, .. } if expires.reached(step.now) => Steady::settled(step),
            _ => self.stay(),
        }
    }

    fn on_total_changed(self, step: &mut Step<'_>, changed: bool) -> Phase {
        match self.stage {
            Stage::Settling { target, .. } if step.limits().total_mem() >= target => {
                Steady::settled(step)
            }
            _ => {
                if changed {
                    step.world.rebrake(step.fx);
                }

                self.stay()
            }
        }
    }

    fn on_grant(self, step: &mut Step<'_>, id: u64, target: u64) -> Phase {
        if !self.answers(id) {
            return self.stay();
        }

        let expires = Deadline::after(step.now, step.config().settle_grace.get());
        let settling = Self::at(self.exhausted_since, Stage::Settling { target, expires });

        settling.on_total_changed(step, false)
    }

    fn on_deny(
        self,
        step: &mut Step<'_>,
        id: u64,
        reason: DenyReason,
        retry_after: Option<Duration>,
    ) -> Phase {
        if !self.answers(id) {
            return self.stay();
        }

        if reason == DenyReason::Ceiling || step.world.grow_target().is_none() {
            return Exhausted::enter(step, self.exhausted_since);
        }

        let backoff = retry_after.unwrap_or_else(|| step.config().deny_backoff.get());

        debug!(
            id,
            ?reason,
            ?backoff,
            "backing off before retrying the request"
        );

        Self::at(
            self.exhausted_since,
            Stage::Backoff {
                retry_at: Deadline::after(step.now, backoff),
            },
        )
    }

    fn on_connected(self, _step: &mut Step<'_>) -> Phase {
        self.blocked()
    }

    fn on_disconnected(self, _step: &mut Step<'_>) -> Phase {
        self.blocked()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pazari_proto::{DenyReason, GuestMessage, HostMessage};

    use crate::engine::{Action, Event, Phase, testing::*};

    #[test]
    fn a_brake_inside_the_escalation_window_doubles_the_next_ask() {
        let mut engine = started();

        grown(&mut engine);

        assert_eq!(
            wake(&mut engine, at(110), 2),
            vec![request(2, 2560 * MIB, 3584 * MIB),],
        );
    }

    #[test]
    fn a_brake_after_the_escalation_window_asks_for_one_plain_step() {
        let mut engine = started();

        grown(&mut engine);

        assert_eq!(
            wake(&mut engine, at(200), 2),
            vec![request(2, 2560 * MIB, 3072 * MIB),],
        );
    }

    #[test]
    fn the_request_target_covers_the_deficit_and_rounds_up_to_a_block() {
        let mut engine = started();

        tick(&mut engine, at(60), snap(BOOT, 2200 * MIB, 0));

        assert_eq!(
            wake(&mut engine, at(61), 1),
            vec![request(1, BOOT, 2816 * MIB)]
        );
    }

    #[test]
    fn the_request_target_is_clamped_and_aligned_under_the_ceiling() {
        let mut engine = started_with(config(), 2200 * MIB);

        assert_eq!(
            wake(&mut engine, at(100), 1),
            vec![request(1, BOOT, 2176 * MIB)]
        );
    }

    #[test]
    fn a_grant_wait_holds_the_brake_where_it_stands() {
        let mut engine = started();

        wake(&mut engine, at(100), 1);

        assert_eq!(engine.pending_request(), Some(1));
        assert_eq!(engine.high(), HIGH0);
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
        assert!(matches!(engine.phase(), Phase::Growing(_)));

        assert_eq!(
            engine.handle(Event::TotalChanged {
                now: at(102),
                total_mem: 2560 * MIB,
            }),
            vec![Action::SetMax(2304 * MIB), Action::SetHigh(2048 * MIB)]
        );
        assert!(matches!(engine.phase(), Phase::Steady(_)));
        assert_eq!(engine.total_mem(), 2560 * MIB);
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
            engine.handle(Event::TotalChanged {
                now: at(102),
                total_mem: 2304 * MIB,
            }),
            vec![Action::SetMax(2048 * MIB), Action::SetHigh(1792 * MIB)]
        );
        assert!(matches!(engine.phase(), Phase::Steady(_)));
        assert_eq!(engine.max(), 2048 * MIB);
        assert_eq!(engine.high(), 1792 * MIB);
    }

    #[test]
    fn a_partial_plug_settles_from_the_total_it_reached_after_the_grace() {
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
            engine.handle(Event::TotalChanged {
                now: at(102),
                total_mem: 2304 * MIB,
            }),
            vec![Action::SetMax(2048 * MIB), Action::SetHigh(1792 * MIB)]
        );
        assert!(matches!(engine.phase(), Phase::Growing(_)));

        assert_eq!(
            tick(&mut engine, at(110), snap(2304 * MIB, BUSY, 1)),
            vec![]
        );
        assert_eq!(
            tick(&mut engine, at(111), snap(2304 * MIB, BUSY, 1)),
            vec![]
        );
        assert!(matches!(engine.phase(), Phase::Steady(_)));
        assert_eq!(engine.high(), 1792 * MIB);
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
        assert!(matches!(engine.phase(), Phase::Growing(_)));
    }
}
