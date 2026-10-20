use std::time::Duration;

use pazari_proto::{DenyReason, GuestMessage};
use pazari_types::Deadline;
use tracing::debug;

use crate::engine::{
    action::Action,
    counters::Pressure,
    phase::{Phase, State, Step, growing::Growing, steady::Steady},
    world::RequestId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Owed(u64);

impl Owed {
    pub const fn new(id: u64) -> Self {
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
    const fn refusal(self) -> Option<&'static str> {
        match self {
            Self::Offered | Self::Ended => None,
            Self::Spent => Some("the reclaim left nothing the guest can spare"),
            Self::Braked => Some("the workload throttled before the reclaim landed"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Reclaiming { bytes: u64, owed: Option<Owed> },
    Offering { id: RequestId, target: u64 },
    Settling { target: u64, expires: Deadline },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Releasing {
    stage: Stage,
}

impl Releasing {
    pub fn begin(step: &mut Step<'_>, bytes: u64, owed: Option<Owed>) -> Phase {
        let swappiness = step.config().reclaim_swappiness;

        debug!(bytes, ?swappiness, "reclaiming from the workload cgroup");

        step.emit(Action::StartReclaim { bytes, swappiness });

        Phase::Releasing(Self {
            stage: Stage::Reclaiming { bytes, owed },
        })
    }

    pub const fn pending_request(self) -> Option<u64> {
        match self.stage {
            Stage::Offering { id, .. } => Some(id.get()),
            _ => None,
        }
    }

    const fn answers(self, id: u64) -> bool {
        matches!(self.pending_request(), Some(pending) if pending == id)
    }

    fn answer(self, step: &mut Step<'_>, outcome: Outcome) {
        let Stage::Reclaiming {
            owed: Some(owed), ..
        } = self.stage
        else {
            return;
        };

        if let Some(reason) = outcome.refusal() {
            step.nak(owed.0, reason);
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

    fn busy(self) -> &'static str {
        "a release is already in progress"
    }

    fn brake(self, step: &mut Step<'_>) -> Phase {
        if !matches!(self.stage, Stage::Reclaiming { .. }) {
            return self.stay();
        }

        self.answer(step, Outcome::Braked);

        step.emit(Action::CancelReclaim);

        Growing::braked(step)
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

    fn on_total_changed(self, step: &mut Step<'_>, changed: bool) -> Phase {
        match self.stage {
            Stage::Settling { target, .. } if step.limits().total_mem() <= target => {
                self.finish(step, Outcome::Offered)
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

        Self::settling(target, expires).on_total_changed(step, false)
    }

    fn on_deny(
        self,
        step: &mut Step<'_>,
        id: u64,
        _reason: DenyReason,
        _retry_after: Option<Duration>,
    ) -> Phase {
        if !self.answers(id) {
            return self.stay();
        }

        self.finish(step, Outcome::Offered)
    }

    fn on_reclaim_finished(self, step: &mut Step<'_>, reclaimed: u64) -> Phase {
        let Stage::Reclaiming { bytes, .. } = self.stage else {
            return self.stay();
        };

        if !step.world.connected() {
            return self.finish(step, Outcome::Ended);
        }

        let releasable = step
            .world
            .release_budget_after_reclaim(bytes, reclaimed)
            .unwrap_or(0);

        if releasable == 0 {
            return self.finish(step, Outcome::Spent);
        }

        self.answer(step, Outcome::Offered);

        let id = step.world.take_id();
        let current = step.limits().total_mem();

        debug!(
            id = id.get(),
            current,
            releasable,
            asked = bytes,
            reclaimed,
            "offering memory back to the host",
        );

        step.send(GuestMessage::OfferRelease {
            id: id.get(),
            current,
            releasable,
        });

        Phase::Releasing(Self {
            stage: Stage::Offering {
                id,
                target: current.saturating_sub(releasable),
            },
        })
    }

    fn on_connected(self, step: &mut Step<'_>) -> Phase {
        Steady::released(step)
    }

    fn on_disconnected(self, step: &mut Step<'_>) -> Phase {
        self.finish(step, Outcome::Ended)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pazari_proto::{DenyReason, GuestMessage, HostMessage};
    use pazari_types::Swappiness;

    use crate::engine::{Action, Event, Phase, testing::*};

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
                    id: 7,
                    desired: 512 * MIB,
                    deadline_ms: Duration::from_secs(30),
                },
            ),
            vec![
                Action::Send(GuestMessage::ReclaimAck {
                    id: 7,
                    will_release: 512 * MIB,
                }),
                Action::StartReclaim {
                    bytes: 512 * MIB,
                    swappiness: Some(Swappiness::NONE),
                },
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
                [Action::Send(GuestMessage::ReclaimNak { id: 7, .. })]
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
                id: 7,
                desired: 512 * MIB,
                deadline_ms: Duration::from_secs(30),
            },
        );

        let actions = wake(&mut engine, at(142), 2);

        assert!(
            actions.iter().any(|action| matches!(
                action,
                Action::Send(GuestMessage::ReclaimNak { id: 7, .. })
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
                id: 2,
                current: 2560 * MIB,
                releasable: 256 * MIB,
            })],
        );
        assert!(matches!(engine.phase(), Phase::Releasing(_)));
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
                    id: 2,
                    reason: DenyReason::Policy,
                    retry_after_ms: None,
                },
            ),
            vec![]
        );
        assert!(matches!(engine.phase(), Phase::Steady(_)));
        assert_eq!(engine.total_mem(), 2560 * MIB);
    }
}
