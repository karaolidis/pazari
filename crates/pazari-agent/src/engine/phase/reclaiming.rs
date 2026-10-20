use std::num::NonZeroU64;

use pazari_proto::ReclaimId;
use pazari_types::Timestamp;
use tracing::debug;

use crate::engine::{
    phase::{Phase, State, Step, growing::Growing, steady::Steady},
    refusal::Refusal,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Waiting for a reclaim already running. What it frees counts towards
    /// `wanted`.
    Waiting {
        wanted: NonZeroU64,
    },

    Running {
        asked: NonZeroU64,
    },
}

/// Reclaiming from a throttled workload before asking the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reclaiming {
    braked: Timestamp,
    stage: Stage,
}

impl Reclaiming {
    pub fn begin(step: &mut Step<'_>, amount: NonZeroU64) -> Phase {
        Self::run(step, step.now, amount)
    }

    pub const fn waiting(step: &Step<'_>, amount: NonZeroU64) -> Phase {
        Phase::Reclaiming(Self {
            braked: step.now,
            stage: Stage::Waiting { wanted: amount },
        })
    }

    fn run(step: &mut Step<'_>, braked: Timestamp, amount: NonZeroU64) -> Phase {
        debug!(
            amount = amount.get(),
            slack = ?step.world.slack(),
            usage = ?step.guest().usage(),
            "reclaiming from the workload before asking the host",
        );

        step.world.start_reclaim(amount, step.fx);

        Phase::Reclaiming(Self {
            braked,
            stage: Stage::Running { asked: amount },
        })
    }
}

impl State for Reclaiming {
    fn stay(self) -> Phase {
        Phase::Reclaiming(self)
    }

    fn brake(self, _step: &mut Step<'_>) -> Phase {
        self.stay()
    }

    fn resume(self, _step: &mut Step<'_>) -> Phase {
        self.stay()
    }

    fn on_reclaim_request(self, step: &mut Step<'_>, id: ReclaimId, _desired: u64) -> Phase {
        step.nak(id, Refusal::Reclaiming);

        self.stay()
    }

    fn on_reclaim_finished(self, step: &mut Step<'_>, reclaimed: u64) -> Phase {
        match self.stage {
            Stage::Waiting { wanted } => {
                let Some(left) = NonZeroU64::new(wanted.get().saturating_sub(reclaimed)) else {
                    return Steady::made_room(step);
                };

                if step.world.throttle_reclaim().is_none() {
                    return Growing::braked_at(step, self.braked);
                }

                Self::run(step, self.braked, left)
            }
            Stage::Running { asked } if reclaimed >= asked.get() => {
                debug!(reclaimed, "the reclaim made room without the host");

                Steady::made_room(step)
            }
            Stage::Running { asked } => {
                debug!(
                    reclaimed,
                    asked = asked.get(),
                    "the reclaim fell short; asking the host",
                );

                Growing::braked_at(step, self.braked)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pazari_proto::{GuestMessage, HostMessage};

    use crate::engine::{Action, EngineConfig, Event, Phase, ThrottleMode, testing::*};

    const AMOUNT: u64 = 256 * MIB;

    fn reclaiming() -> EngineConfig {
        EngineConfig {
            throttle: ThrottleMode::Reclaim {
                amount: bytes(AMOUNT),
            },
            ..config()
        }
    }

    #[test]
    fn a_throttle_with_memory_to_spare_reclaims_instead_of_asking() {
        let mut engine = started_with(reclaiming(), CEILING);

        assert_eq!(wake(&mut engine, at(100), 1), vec![reclaim(AMOUNT)]);
        assert!(matches!(engine.phase(), Phase::Reclaiming(_)));
        assert_eq!(engine.pending_request(), None);
    }

    #[test]
    fn a_reclaim_that_frees_what_it_asked_for_settles_without_the_host() {
        let mut engine = started_with(reclaiming(), CEILING);
        wake(&mut engine, at(100), 1);

        assert_eq!(reclaimed(&mut engine, at(101), AMOUNT), vec![]);
        assert!(matches!(engine.phase(), Phase::Steady(_)));
        assert_eq!(engine.total_mem(), BOOT);
    }

    #[test]
    fn a_reclaim_that_falls_short_asks_the_host() {
        let mut engine = started_with(reclaiming(), CEILING);
        wake(&mut engine, at(100), 1);

        assert_eq!(
            reclaimed(&mut engine, at(101), 64 * MIB),
            vec![request(1, BOOT, 2560 * MIB)]
        );
        assert!(matches!(engine.phase(), Phase::Growing(_)));
    }

    #[test]
    fn a_throttle_without_memory_to_spare_asks_the_host_at_once() {
        let mut engine = started_with(reclaiming(), CEILING);

        tick(&mut engine, at(99), snap(BOOT, STALLED, 0));

        assert_eq!(
            wake(&mut engine, at(100), 1),
            vec![request(1, BOOT, 2560 * MIB)]
        );
    }

    #[test]
    fn a_throttle_while_reclaiming_starts_no_second_reclaim() {
        let mut engine = started_with(reclaiming(), CEILING);
        wake(&mut engine, at(100), 1);

        assert_eq!(wake(&mut engine, at(101), 2), vec![]);
        assert!(matches!(engine.phase(), Phase::Reclaiming(_)));
    }

    #[test]
    fn a_host_reclaim_request_while_reclaiming_is_refused() {
        let mut engine = started_with(reclaiming(), CEILING);
        wake(&mut engine, at(100), 1);

        assert!(matches!(
            host(
                &mut engine,
                at(101),
                HostMessage::ReclaimRequest {
                    id: ReclaimId(7),
                    desired: 512 * MIB,
                    deadline: Duration::from_secs(30),
                },
            )
            .as_slice(),
            [Action::Send(GuestMessage::ReclaimNak {
                id: ReclaimId(7),
                ..
            })]
        ));
    }

    #[test]
    fn a_throttle_mid_release_waits_out_the_cancelled_reclaim_and_counts_it() {
        let mut engine = started_with(reclaiming(), CEILING);
        releasing(&mut engine);

        assert_eq!(wake(&mut engine, at(144), 2), vec![Action::CancelReclaim]);
        assert!(matches!(engine.phase(), Phase::Reclaiming(_)));

        assert_eq!(
            reclaimed(&mut engine, at(145), 64 * MIB),
            vec![reclaim(AMOUNT - 64 * MIB)]
        );
        assert_eq!(reclaimed(&mut engine, at(146), AMOUNT - 64 * MIB), vec![]);
        assert!(matches!(engine.phase(), Phase::Steady(_)));
    }

    #[test]
    fn a_cancelled_reclaim_that_freed_enough_needs_no_other() {
        let mut engine = started_with(reclaiming(), CEILING);
        releasing(&mut engine);
        wake(&mut engine, at(144), 2);

        assert_eq!(reclaimed(&mut engine, at(145), AMOUNT), vec![]);
        assert!(matches!(engine.phase(), Phase::Steady(_)));
    }

    #[test]
    fn a_disconnect_mid_release_does_not_let_a_throttle_start_a_second_reclaim() {
        let mut engine = started_with(reclaiming(), CEILING);
        releasing(&mut engine);

        assert_eq!(
            engine.handle(Event::Disconnected { now: at(144) }),
            vec![Action::CancelReclaim]
        );
        assert!(matches!(engine.phase(), Phase::Steady(_)));

        assert_eq!(wake(&mut engine, at(145), 2), vec![]);
        assert!(matches!(engine.phase(), Phase::Reclaiming(_)));

        assert_eq!(reclaimed(&mut engine, at(146), 0), vec![reclaim(AMOUNT)]);
    }

    #[test]
    fn a_shortfall_escalates_from_the_throttle_not_from_the_reclaim() {
        let mut engine = started_with(reclaiming(), CEILING);
        tick(&mut engine, at(99), snap(BOOT, STALLED, 0));
        grown(&mut engine);
        tick(&mut engine, at(103), snap(2560 * MIB, CALM, 1));

        assert_eq!(wake(&mut engine, at(105), 2), vec![reclaim(AMOUNT)]);

        assert_eq!(
            reclaimed(&mut engine, at(120), 0),
            vec![request(2, 2560 * MIB, 3584 * MIB)]
        );
    }

    #[test]
    fn a_waiting_reclaim_asks_the_host_once_the_slack_is_gone() {
        let mut engine = started_with(reclaiming(), CEILING);
        releasing(&mut engine);

        wake(&mut engine, at(144), 2);
        tick(&mut engine, at(145), snap(2560 * MIB, 1948 * MIB, 2));

        assert_eq!(
            reclaimed(&mut engine, at(146), 0),
            vec![request(2, 2560 * MIB, 3072 * MIB)]
        );
    }
}
