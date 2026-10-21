use std::time::Duration;

use pazari_proto::ReclaimId;
use pazari_types::{Deadline, Timestamp};
use tracing::warn;

use crate::engine::{
    config::ExhaustionMode,
    counters::Pressure,
    limits::{Brake, Moved},
    phase::{
        Phase, State, Step,
        growing::{Attempt, Growing},
        steady::Steady,
    },
    refusal::Refusal,
    transition::Transition,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Under the brake, asking the host again at `retry_at`.
    Stalling { retry_at: Deadline },

    /// The brake is off, and the agent has stopped asking.
    Collapsed,
}

impl Stage {
    const fn brake(self) -> Brake {
        match self {
            Self::Stalling { .. } => Brake::Engaged,
            Self::Collapsed => Brake::Released,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exhausted {
    since: Timestamp,
    stage: Stage,
}

impl Exhausted {
    /// Exhausts the workload's ceiling, retrying after `backoff` while it stalls.
    pub fn enter(step: &mut Step<'_>, attempt: Attempt, backoff: Duration) -> Phase {
        let since = match attempt {
            Attempt::First => {
                warn!(
                    usage = ?step.guest().usage(),
                    total_mem = step.limits().total_mem(),
                    ceiling = ?step.limits().ceiling(),
                    "workload demand exceeds its ceiling",
                );

                step.fx.record(Transition::Exhaustion);

                step.now
            }
            Attempt::Retry { exhausted_since } => exhausted_since,
        };

        let stage = if Self::stalling(step, since) {
            Stage::Stalling {
                retry_at: Deadline::after(step.now, backoff),
            }
        } else {
            if !step.limits().released() {
                warn!(
                    usage = ?step.guest().usage(),
                    total_mem = step.limits().total_mem(),
                    exhaustion = ?step.config().exhaustion,
                    "at the ceiling with nothing left to request; releasing the brake",
                );
            }

            Stage::Collapsed
        };

        Self::at(step, since, stage)
    }

    fn at(step: &mut Step<'_>, since: Timestamp, stage: Stage) -> Phase {
        step.brake(stage.brake());

        Phase::Exhausted(Self { since, stage })
    }

    /// Whether an exhaustion that began at `since` is still inside its stall.
    pub fn stalling(step: &Step<'_>, since: Timestamp) -> bool {
        match step.config().exhaustion {
            ExhaustionMode::Stall { timeout } => {
                !Deadline::after(since, timeout.get()).reached(step.now)
            }
            ExhaustionMode::Kill => false,
        }
    }

    const fn retrying(self) -> Attempt {
        Attempt::Retry {
            exhausted_since: self.since,
        }
    }

    fn retry(self, step: &mut Step<'_>) -> Phase {
        if step.world.connected() && step.world.grow_target().is_some() {
            return Growing::start(step, self.retrying());
        }

        Phase::Exhausted(Self {
            stage: Stage::Stalling {
                retry_at: Deadline::after(step.now, step.config().deny_backoff.get()),
            },
            ..self
        })
    }
}

impl State for Exhausted {
    fn stay(self) -> Phase {
        Phase::Exhausted(self)
    }

    fn on_connected(self, step: &mut Step<'_>) -> Phase {
        Growing::start(step, self.retrying())
    }

    fn advance(self, step: &mut Step<'_>, _pressure: Pressure) -> Phase {
        let Stage::Stalling { retry_at } = self.stage else {
            return self.stay();
        };

        if !Self::stalling(step, self.since) {
            warn!(
                usage = ?step.guest().usage(),
                total_mem = step.limits().total_mem(),
                exhaustion = ?step.config().exhaustion,
                "workload stalled for its whole timeout; releasing the brake",
            );

            return Self::at(step, self.since, Stage::Collapsed);
        }

        if retry_at.reached(step.now) && !step.world.calm() {
            return self.retry(step);
        }

        self.stay()
    }

    fn on_total_changed(self, step: &mut Step<'_>, moved: Moved) -> Phase {
        match moved {
            Moved::Grew => Steady::settled(step),
            Moved::Shrank | Moved::Unchanged => {
                step.rebrake(moved);

                self.stay()
            }
        }
    }

    fn on_interval(self, step: &mut Step<'_>, _pressure: Pressure) -> Phase {
        if !step.world.calm() {
            return self.stay();
        }

        Steady::recovered(step)
    }

    fn on_reclaim_request(self, step: &mut Step<'_>, id: ReclaimId, _desired: u64) -> Phase {
        step.nak(id, Refusal::Exhausted);

        self.stay()
    }

    fn on_ceiling_raised(self, step: &mut Step<'_>) -> Phase {
        Growing::start(step, Attempt::First)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pazari_proto::{DenyReason, HostMessage};

    use crate::engine::{
        Action, Engine, EngineConfig, Event, ExhaustionMode, Phase, ReleaseConfig, TitrateConfig,
        Transition, testing::*,
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
    fn a_retry_the_host_turns_down_for_pressure_still_times_out() {
        let mut engine = started_with(config(), 2560 * MIB);
        let mut transitions = Vec::new();
        let mut handle = |engine: &mut Engine, event| {
            let outcome = engine.handle(event);
            transitions.extend(outcome.transitions);
            outcome.actions
        };

        handle(
            &mut engine,
            Event::ThrottleWake {
                now: at(100),
                high_events: 1,
                max_events: 0,
            },
        );
        handle(
            &mut engine,
            Event::Host {
                now: at(100),
                message: deny(RequestId(1)),
            },
        );

        let mut asked = Vec::new();
        for second in 101..=130 {
            let actions = handle(
                &mut engine,
                Event::Tick {
                    now: at(second),
                    snapshot: snap(BOOT, STALLED, 1),
                },
            );

            if let Some(id) = engine.pending_request() {
                asked.push(second);
                handle(
                    &mut engine,
                    Event::Host {
                        now: at(second),
                        message: HostMessage::Deny {
                            id,
                            reason: DenyReason::HostPressure,
                            retry_after: Some(Duration::from_secs(1)),
                        },
                    },
                );
            } else if second == 112 {
                assert_eq!(actions, vec![Action::SetHigh(MAX0)]);
            }
        }

        assert_eq!(asked, (105..=111).collect::<Vec<_>>());
        assert!(matches!(engine.phase(), Phase::Exhausted(_)));
        assert!(engine.brake_released());
        assert_eq!(
            transitions,
            vec![Transition::Exhaustion],
            "each refused retry counted as another exhaustion"
        );
    }

    #[test]
    fn a_retry_the_host_never_answers_still_times_out() {
        let mut engine = started_with(config(), 2560 * MIB);

        wake(&mut engine, at(100), 1);
        host(&mut engine, at(100), deny(RequestId(1)));
        tick(&mut engine, at(105), snap(BOOT, STALLED, 1));
        assert_eq!(engine.pending_request(), Some(RequestId(2)));

        for second in 106..=111 {
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
        assert!(matches!(engine.phase(), Phase::Exhausted(_)));
        assert!(engine.brake_released());
    }

    #[test]
    fn a_calm_guest_recovers_while_a_retry_waits_on_the_host() {
        let config = EngineConfig {
            exhaustion: ExhaustionMode::Stall {
                timeout: interval(600),
            },
            ..config()
        };
        let mut engine = started_with(config, 2560 * MIB);

        wake(&mut engine, at(100), 1);
        host(&mut engine, at(100), deny(RequestId(1)));
        tick(&mut engine, at(105), snap(BOOT, STALLED, 1));
        assert_eq!(engine.pending_request(), Some(RequestId(2)));

        for second in 106..=108 {
            tick(&mut engine, at(second), snap(BOOT, CALM, 1));
        }

        assert!(matches!(engine.phase(), Phase::Steady(_)));
    }

    #[test]
    fn a_disconnect_mid_retry_keeps_the_stall_timing() {
        let mut engine = started_with(config(), 2560 * MIB);

        wake(&mut engine, at(100), 1);
        host(&mut engine, at(100), deny(RequestId(1)));
        tick(&mut engine, at(105), snap(BOOT, STALLED, 1));
        assert!(matches!(engine.phase(), Phase::Growing(_)));

        engine.handle(Event::Disconnected { now: at(106) });
        assert!(matches!(engine.phase(), Phase::Exhausted(_)));

        assert_eq!(
            tick(&mut engine, at(112), snap(BOOT, STALLED, 1)),
            vec![Action::SetHigh(MAX0)]
        );
    }

    #[test]
    fn a_shrink_keeps_the_stall_and_lowers_the_limits() {
        let mut engine = started_with(config(), BOOT);

        wake(&mut engine, at(100), 1);
        assert!(matches!(engine.phase(), Phase::Exhausted(_)));

        assert_eq!(
            engine
                .handle(Event::TotalChanged {
                    now: at(101),
                    total_mem: BOOT - BLOCK,
                })
                .actions,
            vec![Action::SetHigh(HIGH0 - BLOCK), Action::SetMax(MAX0 - BLOCK)]
        );
        assert!(matches!(engine.phase(), Phase::Exhausted(_)));
        assert!(!engine.brake_released());
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
            engine
                .handle(Event::TotalChanged {
                    now: at(202),
                    total_mem: 2560 * MIB,
                })
                .actions,
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
            engine
                .handle(Event::TotalChanged {
                    now: at(200),
                    total_mem: 2560 * MIB,
                })
                .actions,
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
            recover_after: count(5),
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
