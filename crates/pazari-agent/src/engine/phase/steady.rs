use pazari_proto::{GuestMessage, ReclaimId};
use tracing::debug;

use crate::engine::{
    counters::Pressure,
    limits::Brake,
    phase::{
        Phase, State, Step,
        releasing::{Owed, Releasing},
    },
    refusal::Refusal,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Steady;

impl Steady {
    pub const fn start() -> Self {
        Self
    }

    pub fn settled(step: &mut Step<'_>) -> Phase {
        step.world.note_grown(step.now);

        Self::released(step)
    }

    pub fn released(step: &mut Step<'_>) -> Phase {
        step.world.note_rested(step.now);

        Self::enter(step)
    }

    pub fn made_room(step: &mut Step<'_>) -> Phase {
        Self::enter(step)
    }

    pub fn recovered(step: &mut Step<'_>) -> Phase {
        step.world.note_recovered();

        Self::enter(step)
    }

    fn enter(step: &mut Step<'_>) -> Phase {
        step.brake(Brake::Engaged);

        Phase::Steady(Self)
    }
}

impl State for Steady {
    fn stay(self) -> Phase {
        Phase::Steady(self)
    }

    fn brake(self, step: &mut Step<'_>) -> Phase {
        Phase::braked(step)
    }

    fn resume(self, _step: &mut Step<'_>) -> Phase {
        self.stay()
    }

    fn on_reclaim_request(self, step: &mut Step<'_>, id: ReclaimId, desired: u64) -> Phase {
        if step.world.reclaiming() {
            step.nak(id, Refusal::ReclaimPending);

            return self.stay();
        }

        let will_release = step.world.release_budget(desired);

        debug!(
            %id,
            desired,
            ?will_release,
            usage = ?step.guest().usage(),
            total_mem = step.limits().total_mem(),
            "answering the host's reclaim request",
        );

        let Some(will_release) = will_release else {
            step.nak(id, Refusal::NoHeadroom);

            return self.stay();
        };

        step.send(GuestMessage::ReclaimAck {
            id,
            will_release: will_release.get(),
        });

        Releasing::begin(step, will_release, Some(Owed::new(id)))
    }

    fn on_interval(self, step: &mut Step<'_>, pressure: Pressure) -> Phase {
        let Some(desired) = step.world.release_due(step.now, pressure) else {
            return self.stay();
        };

        if !step.world.connected() || step.world.reclaiming() {
            return self.stay();
        }

        let Some(bytes) = step.world.release_budget(desired.get()) else {
            debug!(
                available = ?step.guest().available(),
                usage = ?step.guest().usage(),
                total_mem = step.limits().total_mem(),
                "slack has held, but no headroom to release",
            );

            return self.stay();
        };

        debug!(
            bytes = bytes.get(),
            available = ?step.guest().available(),
            usage = ?step.guest().usage(),
            total_mem = step.limits().total_mem(),
            "guest has held reclaimable memory long enough to shrink",
        );

        Releasing::begin(step, bytes, None)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pazari_proto::{GuestMessage, HostMessage};
    use pazari_types::Swappiness;

    use crate::engine::{Action, EngineConfig, Phase, ReleaseConfig, TitrateConfig, testing::*};

    #[test]
    fn a_shrink_needs_exactly_the_configured_release_intervals() {
        let mut engine = started();
        grown(&mut engine);

        for second in 140..=142 {
            assert_eq!(
                tick(&mut engine, at(second), snap(2560 * MIB, CALM, 1)),
                vec![],
                "unexpected action at t+{second}"
            );
        }

        assert_eq!(
            tick(&mut engine, at(143), snap(2560 * MIB, CALM, 1)),
            vec![Action::StartReclaim {
                bytes: 256 * MIB,
                swappiness: Some(Swappiness::NONE),
            }]
        );
        assert!(matches!(engine.phase(), Phase::Releasing(_)));
    }

    #[test]
    fn a_tick_without_slack_resets_the_streak() {
        let mut engine = started();
        grown(&mut engine);

        tick(&mut engine, at(140), snap(2560 * MIB, CALM, 1));
        tick(&mut engine, at(141), snap(2560 * MIB, CALM, 1));

        tick(
            &mut engine,
            at(142),
            snap_available(2560 * MIB, CALM, 600 * MIB, 1),
        );

        for second in 143..=144 {
            assert_eq!(
                tick(&mut engine, at(second), snap(2560 * MIB, CALM, 1)),
                vec![],
                "unexpected action at t+{second}"
            );
        }

        assert_eq!(
            tick(&mut engine, at(145), snap(2560 * MIB, CALM, 1)),
            vec![Action::StartReclaim {
                bytes: 256 * MIB,
                swappiness: Some(Swappiness::NONE),
            }]
        );
    }

    #[test]
    fn a_shrink_waits_out_the_cooldown_after_a_grow() {
        let mut engine = started();
        grown(&mut engine);

        for second in 103..=131 {
            assert_eq!(
                tick(&mut engine, at(second), snap(2560 * MIB, CALM, 1)),
                vec![],
                "unexpected action at t+{second}"
            );
        }

        assert_eq!(
            tick(&mut engine, at(132), snap(2560 * MIB, CALM, 1)),
            vec![Action::StartReclaim {
                bytes: 256 * MIB,
                swappiness: Some(Swappiness::NONE),
            }]
        );
    }

    #[test]
    fn a_workload_that_uses_its_grant_keeps_it() {
        let mut engine = started();
        grown(&mut engine);

        for second in 140..=180 {
            assert_eq!(
                tick(&mut engine, at(second), snap(2560 * MIB, 2300 * MIB, 1)),
                vec![],
                "the guest released memory the workload was using at t+{second}"
            );
        }

        assert!(matches!(engine.phase(), Phase::Steady(_)));
    }

    #[test]
    fn a_breach_holds_off_the_next_release() {
        let mut engine = started();
        grown(&mut engine);

        tick(
            &mut engine,
            at(140),
            snap_breached(2560 * MIB, 2000 * MIB, 1),
        );

        for second in 141..=169 {
            assert_eq!(
                tick(&mut engine, at(second), snap(2560 * MIB, 0, 1)),
                vec![],
                "the guest released in the wake of a breach at t+{second}"
            );
        }

        assert!(matches!(engine.phase(), Phase::Steady(_)));
    }

    #[test]
    fn a_release_never_goes_below_boot_memory() {
        let mut engine = started();

        for second in 100..=110 {
            assert_eq!(
                tick(&mut engine, at(second), snap(BOOT, CALM, 0)),
                vec![],
                "unexpected action at t+{second}"
            );
        }

        assert!(matches!(engine.phase(), Phase::Steady(_)));
    }

    #[test]
    fn a_release_is_clamped_to_the_boot_memory_floor() {
        let config = EngineConfig {
            release: ReleaseConfig::Titrate(TitrateConfig {
                step: bytes(1024 * MIB),
                ..titrate()
            }),
            ..config()
        };
        let mut engine = started_with(config, CEILING);
        grown(&mut engine);

        for second in 140..=142 {
            tick(&mut engine, at(second), snap(2560 * MIB, CALM, 1));
        }

        assert_eq!(
            tick(&mut engine, at(143), snap(2560 * MIB, CALM, 1)),
            vec![Action::StartReclaim {
                bytes: 512 * MIB,
                swappiness: Some(Swappiness::NONE),
            }]
        );
    }

    #[test]
    fn a_host_reclaim_request_is_acked_and_run() {
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
                Action::StartReclaim {
                    bytes: 512 * MIB,
                    swappiness: Some(Swappiness::NONE),
                },
            ]
        );
        assert!(matches!(engine.phase(), Phase::Releasing(_)));
    }

    #[test]
    fn a_host_reclaim_request_is_naked_without_headroom() {
        let mut engine = started();

        tick(&mut engine, at(100), snap(BOOT, BUSY, 0));

        let actions = host(
            &mut engine,
            at(101),
            HostMessage::ReclaimRequest {
                id: ReclaimId(9),
                desired: 512 * MIB,
                deadline: Duration::from_secs(30),
            },
        );

        assert!(matches!(
            actions.as_slice(),
            [Action::Send(GuestMessage::ReclaimNak {
                id: ReclaimId(9),
                ..
            })]
        ));
        assert!(matches!(engine.phase(), Phase::Steady(_)));
    }
}
