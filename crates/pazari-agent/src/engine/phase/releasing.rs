use std::{num::NonZeroU64, time::Duration};

use pazari_proto::{DenyReason, GuestMessage, ReclaimId, RequestId};
use pazari_types::Deadline;
use tracing::debug;

use crate::engine::{
    counters::Pressure,
    limits::Moved,
    phase::{Phase, State, Step, steady::Steady},
    refusal::Refusal,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Owed(ReclaimId);

impl Owed {
    pub const fn new(id: ReclaimId) -> Self {
        Self(id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Offered,
    Spent,
    Braked,
    Ended,
}

impl Outcome {
    const fn refusal(self) -> Option<Refusal> {
        match self {
            Self::Offered | Self::Ended => None,
            Self::Spent => Some(Refusal::Spent),
            Self::Braked => Some(Refusal::Braked),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Freeing {
        bytes: NonZeroU64,
        owed: Option<Owed>,
    },
    Offering {
        id: RequestId,
        target: u64,
    },
    Settling {
        target: u64,
        expires: Deadline,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Releasing {
    stage: Stage,
}

impl Releasing {
    pub fn begin(step: &mut Step<'_>, bytes: NonZeroU64, owed: Option<Owed>) -> Phase {
        let releasing = Self {
            stage: Stage::Freeing { bytes, owed },
        };

        let Some(reclaim) = step.world.reclaim_for(bytes) else {
            debug!(
                bytes = bytes.get(),
                free = ?step.guest().free(),
                "free memory already covers the release",
            );

            return releasing.on_reclaim_finished(step, 0);
        };

        debug!(
            bytes = bytes.get(),
            reclaim = reclaim.get(),
            free = ?step.guest().free(),
            "reclaiming from the workload cgroup",
        );

        step.world.start_reclaim(reclaim, step.fx);

        Phase::Releasing(releasing)
    }

    pub const fn pending_request(self) -> Option<RequestId> {
        match self.stage {
            Stage::Offering { id, .. } => Some(id),
            _ => None,
        }
    }

    const fn answers(self, id: RequestId) -> bool {
        matches!(self.pending_request(), Some(pending) if pending.get() == id.get())
    }

    fn answer(self, step: &mut Step<'_>, outcome: Outcome) {
        let Stage::Freeing {
            owed: Some(owed), ..
        } = self.stage
        else {
            return;
        };

        if let Some(refusal) = outcome.refusal() {
            step.nak(owed.0, refusal);
        }
    }

    fn finish(self, step: &mut Step<'_>, outcome: Outcome) -> Phase {
        self.answer(step, outcome);

        Steady::released(step)
    }

    const fn settling(target: u64, expires: Deadline) -> Self {
        Self {
            stage: Stage::Settling { target, expires },
        }
    }
}

impl State for Releasing {
    fn stay(self) -> Phase {
        Phase::Releasing(self)
    }

    fn brake(self, step: &mut Step<'_>) -> Phase {
        if !matches!(self.stage, Stage::Freeing { .. }) {
            return self.stay();
        }

        self.answer(step, Outcome::Braked);

        step.world.cancel_reclaim(step.fx);

        Phase::braked(step)
    }

    fn resume(self, _step: &mut Step<'_>) -> Phase {
        self.stay()
    }

    fn advance(self, step: &mut Step<'_>, _pressure: Pressure) -> Phase {
        match self.stage {
            Stage::Settling { expires, .. } if expires.reached(step.now) => {
                self.finish(step, Outcome::Offered)
            }
            _ => self.stay(),
        }
    }

    fn on_total_changed(self, step: &mut Step<'_>, moved: Moved) -> Phase {
        match self.stage {
            Stage::Settling { target, .. } if step.limits().total_mem() <= target => {
                self.finish(step, Outcome::Offered)
            }
            _ => {
                if moved != Moved::Unchanged {
                    step.world.rebrake(step.fx);
                }

                self.stay()
            }
        }
    }

    fn on_grant(self, step: &mut Step<'_>, id: RequestId, target: u64) -> Phase {
        if !self.answers(id) {
            return self.stay();
        }

        let expires = Deadline::after(step.now, step.config().settle_grace.get());

        Self::settling(target, expires).on_total_changed(step, Moved::Unchanged)
    }

    fn on_deny(
        self,
        step: &mut Step<'_>,
        id: RequestId,
        _reason: DenyReason,
        _retry_after: Option<Duration>,
    ) -> Phase {
        if !self.answers(id) {
            return self.stay();
        }

        self.finish(step, Outcome::Offered)
    }

    fn on_reclaim_request(self, step: &mut Step<'_>, id: ReclaimId, _desired: u64) -> Phase {
        step.nak(id, Refusal::Releasing);

        self.stay()
    }

    fn on_reclaim_finished(self, step: &mut Step<'_>, reclaimed: u64) -> Phase {
        let Stage::Freeing { bytes, .. } = self.stage else {
            return self.stay();
        };

        if !step.world.connected() {
            return self.finish(step, Outcome::Ended);
        }

        let Some(releasable) = step.world.release_budget_after_reclaim(bytes, reclaimed) else {
            return self.finish(step, Outcome::Spent);
        };

        self.answer(step, Outcome::Offered);

        let id = step.world.take_id();
        let current = step.limits().total_mem();

        debug!(
            id = id.get(),
            current,
            releasable = releasable.get(),
            asked = bytes.get(),
            reclaimed,
            "offering memory back to the host",
        );

        step.send(GuestMessage::OfferRelease {
            id,
            current,
            releasable: releasable.get(),
        });

        Phase::Releasing(Self {
            stage: Stage::Offering {
                id,
                target: current.saturating_sub(releasable.get()),
            },
        })
    }

    fn on_connected(self, step: &mut Step<'_>) -> Phase {
        step.world.cancel_reclaim(step.fx);

        Steady::released(step)
    }

    fn on_disconnected(self, step: &mut Step<'_>) -> Phase {
        step.world.cancel_reclaim(step.fx);

        self.finish(step, Outcome::Ended)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pazari_proto::{DenyReason, GuestMessage, HostMessage};

    use crate::engine::{Action, EngineConfig, Event, Phase, ReleaseConfig, testing::*};

    #[test]
    fn an_offer_that_collapses_tells_the_host_rather_than_going_quiet() {
        let mut engine = started();
        grown(&mut engine);

        tick(&mut engine, at(140), snap(2560 * MIB, CALM, 1));

        assert_eq!(
            host(
                &mut engine,
                at(141),
                HostMessage::ReclaimRequest {
                    id: ReclaimId(7),
                    desired: 512 * MIB,
                    deadline: Duration::from_secs(30),
                },
            ),
            vec![
                Action::Send(GuestMessage::ReclaimAck {
                    id: ReclaimId(7),
                    will_release: 512 * MIB,
                }),
                reclaim(512 * MIB),
            ]
        );

        tick(
            &mut engine,
            at(142),
            snap_available(2560 * MIB, 2000 * MIB, 512 * MIB, 1),
        );

        assert!(
            matches!(
                engine
                    .handle(Event::ReclaimFinished {
                        now: at(143),
                        reclaimed: 0,
                    })
                    .as_slice(),
                [Action::Send(GuestMessage::ReclaimNak {
                    id: ReclaimId(7),
                    ..
                })]
            ),
            "the guest acked a release and then went quiet on it",
        );
        assert!(matches!(engine.phase(), Phase::Steady(_)));
    }

    #[test]
    fn a_brake_mid_reclaim_tells_the_host_rather_than_going_quiet() {
        let mut engine = started();
        grown(&mut engine);

        tick(&mut engine, at(140), snap(2560 * MIB, CALM, 1));

        host(
            &mut engine,
            at(141),
            HostMessage::ReclaimRequest {
                id: ReclaimId(7),
                desired: 512 * MIB,
                deadline: Duration::from_secs(30),
            },
        );

        let actions = wake(&mut engine, at(142), 2);

        assert!(
            actions.iter().any(|action| matches!(
                action,
                Action::Send(GuestMessage::ReclaimNak {
                    id: ReclaimId(7),
                    ..
                })
            )),
            "the guest acked a release, braked out of it, and went quiet: {actions:?}",
        );
        assert!(
            actions.contains(&Action::CancelReclaim),
            "the guest braked out of a release and left the reclaim running: {actions:?}",
        );
        assert!(matches!(engine.phase(), Phase::Growing(_)));
    }

    #[test]
    fn a_cancelled_reclaim_blocks_a_host_reclaim_until_it_reports() {
        let mut engine = started();
        releasing(&mut engine);

        wake(&mut engine, at(144), 2);
        host(
            &mut engine,
            at(145),
            HostMessage::Grant {
                id: RequestId(2),
                target: 3072 * MIB,
            },
        );
        engine.handle(Event::TotalChanged {
            now: at(146),
            total_mem: 3072 * MIB,
        });
        assert!(matches!(engine.phase(), Phase::Steady(_)));

        let request = HostMessage::ReclaimRequest {
            id: ReclaimId(7),
            desired: 512 * MIB,
            deadline: Duration::from_secs(30),
        };

        assert!(matches!(
            host(&mut engine, at(147), request.clone()).as_slice(),
            [Action::Send(GuestMessage::ReclaimNak {
                id: ReclaimId(7),
                ..
            })]
        ));

        reclaimed(&mut engine, at(148), 0);

        assert!(matches!(
            host(&mut engine, at(149), request).as_slice(),
            [
                Action::Send(GuestMessage::ReclaimAck {
                    id: ReclaimId(7),
                    ..
                }),
                ..
            ]
        ));
    }

    #[test]
    fn an_offer_credits_what_the_reclaim_freed_after_the_last_reading() {
        let mut engine = started();
        grown(&mut engine);

        for second in 140..=143 {
            tick(&mut engine, at(second), snap(2560 * MIB, CALM, 1));
        }

        assert!(matches!(engine.phase(), Phase::Releasing(_)));

        tick(
            &mut engine,
            at(144),
            snap_available(2560 * MIB, 2000 * MIB, 512 * MIB, 1),
        );

        assert_eq!(
            engine.handle(Event::ReclaimFinished {
                now: at(145),
                reclaimed: 256 * MIB,
            }),
            vec![Action::Send(GuestMessage::OfferRelease {
                id: RequestId(2),
                current: 2560 * MIB,
                releasable: 256 * MIB,
            })],
        );
        assert!(matches!(engine.phase(), Phase::Releasing(_)));
    }

    #[test]
    fn free_memory_that_covers_the_release_skips_the_reclaim() {
        let mut engine = started();
        grown(&mut engine);

        for second in 140..=142 {
            tick(&mut engine, at(second), snap(2560 * MIB, CALM, 1));
        }

        assert_eq!(
            tick(
                &mut engine,
                at(143),
                snap_free(2560 * MIB, CALM, 1024 * MIB, 1)
            ),
            vec![Action::Send(GuestMessage::OfferRelease {
                id: RequestId(2),
                current: 2560 * MIB,
                releasable: 256 * MIB,
            })],
        );
        assert!(matches!(engine.phase(), Phase::Releasing(_)));
    }

    #[test]
    fn free_memory_that_covers_part_of_the_release_leaves_the_rest_to_reclaim() {
        let mut engine = started();
        grown(&mut engine);

        for second in 140..=142 {
            tick(&mut engine, at(second), snap(2560 * MIB, CALM, 1));
        }

        assert_eq!(
            tick(
                &mut engine,
                at(143),
                snap_free(2560 * MIB, CALM, 192 * MIB, 1)
            ),
            vec![reclaim(64 * MIB)],
        );
    }

    #[test]
    fn a_sweep_reclaims_everything_spare_in_one_release() {
        let mut engine = started_with(
            EngineConfig {
                release: ReleaseConfig::Sweep(sweep()),
                ..config()
            },
            CEILING,
        );
        grown(&mut engine);

        for second in 140..=145 {
            assert_eq!(
                tick(&mut engine, at(second), snap(2560 * MIB, CALM, 1)),
                vec![],
                "released before the spare held at t+{second}"
            );
        }

        assert_eq!(
            tick(&mut engine, at(146), snap(2560 * MIB, CALM, 1)),
            vec![reclaim(512 * MIB)],
        );
    }

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
                id: RequestId(2),
                current: 2560 * MIB,
                releasable: 256 * MIB,
            })]
        );

        assert_eq!(
            host(
                &mut engine,
                at(145),
                HostMessage::Grant {
                    id: RequestId(2),
                    target: 2304 * MIB,
                },
            ),
            vec![]
        );

        assert_eq!(
            engine.handle(Event::TotalChanged {
                now: at(146),
                total_mem: 2304 * MIB,
            }),
            vec![Action::SetHigh(1792 * MIB), Action::SetMax(2048 * MIB)]
        );
        assert!(matches!(engine.phase(), Phase::Steady(_)));
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
                    id: RequestId(2),
                    reason: DenyReason::Policy,
                    retry_after: None,
                },
            ),
            vec![]
        );
        assert!(matches!(engine.phase(), Phase::Steady(_)));
        assert_eq!(engine.total_mem(), 2560 * MIB);
    }
}
