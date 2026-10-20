use pazari_types::{Deadline, Timestamp};
use tracing::warn;

use crate::engine::{
    config::ExhaustionMode,
    counters::Pressure,
    limits::Brake,
    phase::{Phase, State, Step, growing::Growing, steady::Steady},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exhausted {
    since: Timestamp,
    retry_at: Option<Deadline>,
}

impl Exhausted {
    pub fn enter(step: &mut Step<'_>, exhausted_since: Option<Timestamp>) -> Phase {
        let since = exhausted_since.unwrap_or(step.now);

        if exhausted_since.is_none() {
            warn!(
                usage = step.guest().usage(),
                total_mem = step.limits().total_mem(),
                ceiling = ?step.limits().ceiling(),
                "workload demand exceeds its ceiling",
            );
        }

        let retry_at = if Self::stalling(step, since) {
            Some(Deadline::after(step.now, step.config().deny_backoff.get()))
        } else {
            if !step.limits().released() {
                warn!(
                    usage = step.guest().usage(),
                    total_mem = step.limits().total_mem(),
                    exhaustion = ?step.config().exhaustion,
                    "at the ceiling with nothing left to request; releasing the brake",
                );
            }

            None
        };

        Self::at(step, since, retry_at)
    }

    fn at(step: &mut Step<'_>, since: Timestamp, retry_at: Option<Deadline>) -> Phase {
        step.brake(if retry_at.is_some() {
            Brake::Engaged
        } else {
            Brake::Released
        });

        Phase::Exhausted(Self { since, retry_at })
    }

    fn stalling(step: &Step<'_>, since: Timestamp) -> bool {
        match step.config().exhaustion {
            ExhaustionMode::Stall { timeout } => {
                !Deadline::after(since, timeout.get()).reached(step.now)
            }
            ExhaustionMode::Kill => false,
        }
    }

    fn retry(self, step: &mut Step<'_>) -> Phase {
        if step.world.connected() && step.world.grow_target().is_some() {
            return Growing::start(step, Some(self.since));
        }

        Phase::Exhausted(Self {
            retry_at: Some(Deadline::after(step.now, step.config().deny_backoff.get())),
            ..self
        })
    }
}

impl State for Exhausted {
    fn stay(self) -> Phase {
        Phase::Exhausted(self)
    }

    fn busy(self) -> &'static str {
        "the workload is stalled at its ceiling"
    }

    fn brake(self, _step: &mut Step<'_>) -> Phase {
        self.stay()
    }

    fn resume(self, step: &mut Step<'_>) -> Phase {
        Growing::start(step, Some(self.since))
    }

    fn advance(self, step: &mut Step<'_>, _pressure: Pressure) -> Phase {
        if !Self::stalling(step, self.since) && !step.limits().released() {
            warn!(
                usage = step.guest().usage(),
                total_mem = step.limits().total_mem(),
                exhaustion = ?step.config().exhaustion,
                "workload stalled for its whole timeout; releasing the brake",
            );

            return Self::at(step, self.since, None);
        }

        match self.retry_at {
            Some(at) if at.reached(step.now) => self.retry(step),
            _ => self.stay(),
        }
    }

    fn on_total_changed(self, step: &mut Step<'_>, changed: bool) -> Phase {
        if changed {
            return Steady::settled(step);
        }

        self.stay()
    }

    fn on_interval(self, step: &mut Step<'_>, _pressure: Pressure) -> Phase {
        if !step.world.calm() {
            return self.stay();
        }

        Steady::recovered(step)
    }

    fn on_ceiling_raised(self, step: &mut Step<'_>) -> Option<Phase> {
        Some(Growing::start(step, None))
    }
}

#[cfg(test)]
mod tests {
    use pazari_proto::{DenyReason, HostMessage};

    use crate::engine::{
        Action, EngineConfig, Event, ExhaustionMode, Phase, ReleaseConfig, TitrateConfig,
        testing::*,
    };

    #[test]
    fn a_ceiling_deny_stalls_under_the_brake_and_stops_asking() {
        let mut engine = started_with(config(), 2560 * MIB);

        wake(&mut engine, at(100), 1);
        host(
            &mut engine,
            at(100),
            HostMessage::Deny {
                id: RequestId(1),
                reason: DenyReason::Ceiling,
                retry_after: None,
            },
        );

        assert!(matches!(engine.phase(), Phase::Exhausted(_)));
        assert!(!engine.brake_released());
        assert_eq!(engine.high(), HIGH0);

        let mut asked = Vec::new();
        for second in 101..=130 {
            let actions = tick(&mut engine, at(second), snap(BOOT, STALLED, 1));

            if let Some(id) = engine.pending_request() {
                asked.push(second);
                host(
                    &mut engine,
                    at(second),
                    HostMessage::Deny {
                        id,
                        reason: DenyReason::Ceiling,
                        retry_after: None,
                    },
                );
            } else if second == 112 {
                assert_eq!(actions, vec![Action::SetHigh(MAX0)]);
            } else {
                assert_eq!(actions, vec![], "unexpected action at t+{second}");
            }
        }

        assert_eq!(asked, vec![105, 110]);
        assert!(matches!(engine.phase(), Phase::Exhausted(_)));
        assert!(engine.brake_released());
        assert_eq!(engine.high(), MAX0);
    }

    #[test]
    fn a_raised_ceiling_ends_the_stall() {
        let mut engine = started_with(config(), 2560 * MIB);

        wake(&mut engine, at(100), 1);
        host(
            &mut engine,
            at(100),
            HostMessage::Deny {
                id: RequestId(1),
                reason: DenyReason::Ceiling,
                retry_after: None,
            },
        );

        assert_eq!(
            host(
                &mut engine,
                at(200),
                HostMessage::Ceiling {
                    ceiling: 8192 * MIB
                },
            ),
            vec![request(2, BOOT, 2560 * MIB)]
        );
        assert!(matches!(engine.phase(), Phase::Growing(_)));

        host(
            &mut engine,
            at(201),
            HostMessage::Grant {
                id: RequestId(2),
                target: 2560 * MIB,
            },
        );

        assert_eq!(
            engine.handle(Event::TotalChanged {
                now: at(202),
                total_mem: 2560 * MIB,
            }),
            vec![Action::SetMax(2304 * MIB), Action::SetHigh(2048 * MIB)]
        );
        assert!(matches!(engine.phase(), Phase::Steady(_)));
    }

    #[test]
    fn an_unsolicited_plug_retires_the_stall() {
        let mut engine = started_with(config(), 2560 * MIB);

        wake(&mut engine, at(100), 1);
        host(
            &mut engine,
            at(100),
            HostMessage::Deny {
                id: RequestId(1),
                reason: DenyReason::Ceiling,
                retry_after: None,
            },
        );
        assert!(matches!(engine.phase(), Phase::Exhausted(_)));

        assert_eq!(
            engine.handle(Event::TotalChanged {
                now: at(200),
                total_mem: 2560 * MIB,
            }),
            vec![Action::SetMax(2304 * MIB), Action::SetHigh(2048 * MIB)]
        );
        assert!(matches!(engine.phase(), Phase::Steady(_)));

        assert_eq!(wake(&mut engine, at(201), 2), vec![]);
        assert!(matches!(engine.phase(), Phase::Exhausted(_)));
        assert!(!engine.brake_released());
        assert_eq!(engine.high(), 2048 * MIB);
    }

    #[test]
    fn kill_mode_never_retries_a_ceiling_deny() {
        let config = EngineConfig {
            exhaustion: ExhaustionMode::Kill,
            ..config()
        };
        let mut engine = started_with(config, 2560 * MIB);

        wake(&mut engine, at(100), 1);
        host(
            &mut engine,
            at(100),
            HostMessage::Deny {
                id: RequestId(1),
                reason: DenyReason::Ceiling,
                retry_after: None,
            },
        );

        for second in 101..=200 {
            assert_eq!(
                tick(&mut engine, at(second), snap(BOOT, STALLED, 1)),
                vec![]
            );
        }

        assert!(matches!(engine.phase(), Phase::Exhausted(_)));
        assert_eq!(engine.pending_request(), None);
        assert!(engine.brake_released());
    }

    #[test]
    fn a_target_already_at_the_ceiling_exhausts_without_asking() {
        let mut engine = started_with(config(), BOOT);

        assert_eq!(wake(&mut engine, at(100), 1), vec![]);
        assert!(matches!(engine.phase(), Phase::Exhausted(_)));
        assert_eq!(engine.pending_request(), None);
    }

    #[test]
    fn a_stall_recovers_after_sustained_calm() {
        let mut engine = started_with(config(), BOOT);

        wake(&mut engine, at(100), 1);
        assert!(matches!(engine.phase(), Phase::Exhausted(_)));

        for second in 101..=104 {
            assert_eq!(
                tick(&mut engine, at(second), snap(BOOT, STALLED, 1)),
                vec![],
                "unexpected action at t+{second}"
            );
        }
        assert!(matches!(engine.phase(), Phase::Exhausted(_)));

        assert_eq!(tick(&mut engine, at(105), snap(BOOT, CALM, 1)), vec![]);
        assert_eq!(tick(&mut engine, at(106), snap(BOOT, CALM, 1)), vec![]);
        assert!(matches!(engine.phase(), Phase::Exhausted(_)));

        assert_eq!(tick(&mut engine, at(107), snap(BOOT, CALM, 1)), vec![]);
        assert!(matches!(engine.phase(), Phase::Steady(_)));
        assert!(!engine.brake_released());
        assert_eq!(engine.high(), HIGH0);
    }

    #[test]
    fn recovery_does_not_wait_for_the_release_strategy_to_see_a_step_spare() {
        let config = EngineConfig {
            release: ReleaseConfig::Titrate(TitrateConfig {
                step: bytes(4096 * MIB),
                ..titrate()
            }),
            ..config()
        };
        let mut engine = started_with(config, BOOT);

        wake(&mut engine, at(100), 1);
        assert!(matches!(engine.phase(), Phase::Exhausted(_)));

        for second in 101..=103 {
            tick(&mut engine, at(second), snap(BOOT, CALM, 1));
        }
        assert!(matches!(engine.phase(), Phase::Exhausted(_)));

        tick(&mut engine, at(104), snap(BOOT, CALM, 1));
        assert!(matches!(engine.phase(), Phase::Steady(_)));
    }

    #[test]
    fn recovery_waits_for_the_intervals_it_is_configured_with() {
        let config = EngineConfig {
            recovery_intervals: count(5),
            ..config()
        };
        let mut engine = started_with(config, BOOT);

        wake(&mut engine, at(100), 1);

        for second in 101..=105 {
            tick(&mut engine, at(second), snap(BOOT, CALM, 1));
        }
        assert!(matches!(engine.phase(), Phase::Exhausted(_)));

        tick(&mut engine, at(106), snap(BOOT, CALM, 1));
        assert!(matches!(engine.phase(), Phase::Steady(_)));
    }

    #[test]
    fn recovery_needs_usage_back_under_the_steady_line() {
        let mut engine = started_with(config(), BOOT);

        wake(&mut engine, at(100), 1);

        for second in 101..=140 {
            let actions = tick(&mut engine, at(second), snap(BOOT, STALLED, 1));

            if second == 112 {
                assert_eq!(actions, vec![Action::SetHigh(MAX0)]);
            } else {
                assert_eq!(actions, vec![], "unexpected action at t+{second}");
            }
        }

        assert!(matches!(engine.phase(), Phase::Exhausted(_)));
        assert!(engine.brake_released());
        assert_eq!(engine.high(), MAX0);
    }

    #[test]
    fn a_disconnected_stall_still_collapses() {
        let mut engine = started_with(config(), BOOT);

        wake(&mut engine, at(100), 1);
        engine.handle(Event::Disconnected { now: at(101) });
        assert!(matches!(engine.phase(), Phase::Exhausted(_)));
        assert_eq!(engine.high(), HIGH0);

        for second in 102..=111 {
            assert_eq!(
                tick(&mut engine, at(second), snap(BOOT, STALLED, 1)),
                vec![],
                "unexpected action at t+{second}"
            );
        }

        assert_eq!(
            tick(&mut engine, at(112), snap(BOOT, STALLED, 1)),
            vec![Action::SetHigh(MAX0)]
        );
        assert_eq!(engine.high(), MAX0);
    }

    #[test]
    fn a_raised_ceiling_re_engages_the_brake_after_the_collapse() {
        let mut engine = started_with(config(), BOOT);

        wake(&mut engine, at(100), 1);
        tick(&mut engine, at(112), snap(BOOT, STALLED, 1));
        assert_eq!(engine.high(), MAX0);

        assert_eq!(
            host(
                &mut engine,
                at(113),
                HostMessage::Ceiling {
                    ceiling: 2560 * MIB
                },
            ),
            vec![Action::SetHigh(HIGH0), request(1, BOOT, 2560 * MIB)]
        );
        assert!(matches!(engine.phase(), Phase::Growing(_)));
        assert!(engine.high() < engine.max());
    }

    #[test]
    fn a_calm_guest_recovers_even_while_the_host_keeps_refusing() {
        let config = EngineConfig {
            deny_backoff: interval(2),
            exhaustion: ExhaustionMode::Stall {
                timeout: interval(600),
            },
            ..config()
        };
        let mut engine = started_with(config, BOOT + 128 * MIB);

        wake(&mut engine, at(100), 1);

        let id = engine
            .pending_request()
            .expect("the throttle asked for memory");
        host(&mut engine, at(100), deny(id));

        assert!(matches!(engine.phase(), Phase::Exhausted(_)));

        for second in 101..=140 {
            tick(&mut engine, at(second), snap(BOOT, CALM, 1));

            if let Some(id) = engine.pending_request() {
                host(&mut engine, at(second), deny(id));
            }

            if matches!(engine.phase(), Phase::Steady(_)) {
                return;
            }
        }

        panic!(
            "an idle guest never came back to steady: {:?}",
            engine.phase(),
        );
    }

    #[test]
    fn a_throttle_resets_the_recovery_count() {
        let mut engine = started_with(config(), BOOT);

        wake(&mut engine, at(100), 1);

        assert_eq!(tick(&mut engine, at(101), snap(BOOT, CALM, 1)), vec![]);
        assert_eq!(tick(&mut engine, at(102), snap(BOOT, CALM, 1)), vec![]);
        assert_eq!(tick(&mut engine, at(103), snap(BOOT, CALM, 2)), vec![]);
        assert_eq!(tick(&mut engine, at(104), snap(BOOT, CALM, 2)), vec![]);
        assert_eq!(tick(&mut engine, at(105), snap(BOOT, CALM, 2)), vec![]);
        assert!(matches!(engine.phase(), Phase::Exhausted(_)));

        assert_eq!(tick(&mut engine, at(106), snap(BOOT, CALM, 2)), vec![]);
        assert!(matches!(engine.phase(), Phase::Steady(_)));
        assert_eq!(engine.high(), HIGH0);
    }

    #[test]
    fn kill_mode_recovers_once_the_workload_is_calm() {
        let config = EngineConfig {
            exhaustion: ExhaustionMode::Kill,
            ..config()
        };
        let mut engine = started_with(config, 2560 * MIB);

        wake(&mut engine, at(100), 1);
        host(
            &mut engine,
            at(100),
            HostMessage::Deny {
                id: RequestId(1),
                reason: DenyReason::Ceiling,
                retry_after: None,
            },
        );
        assert!(matches!(engine.phase(), Phase::Exhausted(_)));

        assert_eq!(tick(&mut engine, at(101), snap(BOOT, CALM, 1)), vec![]);

        assert_eq!(tick(&mut engine, at(102), snap(BOOT, CALM, 1)), vec![]);
        assert_eq!(tick(&mut engine, at(103), snap(BOOT, CALM, 1)), vec![]);
        assert_eq!(
            tick(&mut engine, at(104), snap(BOOT, CALM, 1)),
            vec![Action::SetHigh(HIGH0)]
        );
        assert!(matches!(engine.phase(), Phase::Steady(_)));
    }
}
