use pazari_types::Timestamp;

use crate::engine::{
    config::ExhaustionMode,
    context::Context,
    limits::Band,
    phase::{Phase, State, growing::Growing, steady::Steady},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exhausted {
    since: Timestamp,
    retry_at: Option<Timestamp>,
}

impl Exhausted {
    pub fn enter(ctx: &mut Context, now: Timestamp, exhausted_since: Option<Timestamp>) -> Phase {
        let since = exhausted_since.unwrap_or(now);

        let (band, retry_at) = match ctx.config.exhaustion {
            ExhaustionMode::Stall { timeout } if now < since.saturating_add(timeout.get()) => (
                Band::Open,
                Some(now.saturating_add(ctx.config.deny_backoff.get())),
            ),
            ExhaustionMode::Kill | ExhaustionMode::Stall { .. } => (Band::Collapsed, None),
        };

        ctx.apply_limits(band);

        Phase::Exhausted(Self { since, retry_at })
    }

    fn retry(self, ctx: &mut Context, now: Timestamp) -> Phase {
        if ctx.connected {
            return Growing::start(ctx, now, Some(self.since));
        }

        Phase::Exhausted(Self {
            since: self.since,
            retry_at: Some(now.saturating_add(ctx.config.deny_backoff.get())),
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

    fn advance(self, ctx: &mut Context, now: Timestamp) -> Phase {
        if let ExhaustionMode::Stall { timeout } = ctx.config.exhaustion
            && now >= self.since.saturating_add(timeout.get())
            && ctx.limits.band() != Band::Collapsed
        {
            ctx.apply_limits(Band::Collapsed);

            return Phase::Exhausted(Self {
                since: self.since,
                retry_at: None,
            });
        }

        match self.retry_at {
            Some(at) if now >= at => self.retry(ctx, now),
            _ => self.stay(),
        }
    }

    fn brake(self, _ctx: &mut Context, _now: Timestamp) -> Phase {
        self.stay()
    }

    fn on_plugged(self, ctx: &mut Context, now: Timestamp, changed: bool) -> Phase {
        if changed {
            return Steady::settle(ctx, now);
        }

        self.stay()
    }

    fn on_interval(self, ctx: &mut Context, _now: Timestamp, throttled: bool) -> Phase {
        if !ctx.quiet_streak(throttled) {
            return self.stay();
        }

        ctx.idle.reset();

        Steady::recover(ctx)
    }

    fn on_ceiling_raised(self, ctx: &mut Context, now: Timestamp) -> Option<Phase> {
        Some(Growing::start(ctx, now, None))
    }

    fn resume(self, ctx: &mut Context, now: Timestamp) -> Phase {
        Growing::start(ctx, now, Some(self.since))
    }
}

#[cfg(test)]
mod tests {
    use pazari_proto::{DenyReason, HostMessage};

    use crate::engine::{Action, EngineConfig, Event, ExhaustionMode, Mode, testing::*};

    #[test]
    fn a_ceiling_deny_stalls_with_the_band_open_and_stops_asking() {
        let mut engine = titrated(config(), 2560 * MIB);

        wake(&mut engine, at(100), 1);
        host(
            &mut engine,
            at(100),
            HostMessage::Deny {
                id: 1,
                reason: DenyReason::Ceiling,
                retry_after_ms: None,
            },
        );

        assert_eq!(engine.mode(), Mode::Exhausted);
        assert!(engine.band_open());
        assert_eq!(engine.high(), OPEN0);

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
                        retry_after_ms: None,
                    },
                );
            } else if second == 112 {
                assert_eq!(actions, vec![Action::SetHigh(MAX0)]);
            } else {
                assert_eq!(actions, vec![], "unexpected action at t+{second}");
            }
        }

        assert_eq!(asked, vec![105, 110]);
        assert_eq!(engine.mode(), Mode::Exhausted);
        assert!(engine.band_open());
        assert_eq!(engine.high(), MAX0);
    }

    #[test]
    fn a_raised_ceiling_ends_the_stall() {
        let mut engine = titrated(config(), 2560 * MIB);

        wake(&mut engine, at(100), 1);
        host(
            &mut engine,
            at(100),
            HostMessage::Deny {
                id: 1,
                reason: DenyReason::Ceiling,
                retry_after_ms: None,
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
        assert_eq!(engine.mode(), Mode::AwaitingGrant);

        host(
            &mut engine,
            at(201),
            HostMessage::Grant {
                id: 2,
                target: 2560 * MIB,
            },
        );

        assert_eq!(
            engine.handle(Event::PluggedChanged {
                now: at(202),
                plugged: 2560 * MIB,
            }),
            vec![Action::SetMax(2304 * MIB), Action::SetHigh(2048 * MIB)]
        );
        assert_eq!(engine.mode(), Mode::Steady);
    }

    #[test]
    fn an_unsolicited_plug_retires_the_stall() {
        let mut engine = titrated(config(), 2560 * MIB);

        wake(&mut engine, at(100), 1);
        host(
            &mut engine,
            at(100),
            HostMessage::Deny {
                id: 1,
                reason: DenyReason::Ceiling,
                retry_after_ms: None,
            },
        );
        assert_eq!(engine.mode(), Mode::Exhausted);

        assert_eq!(
            engine.handle(Event::PluggedChanged {
                now: at(200),
                plugged: 2560 * MIB,
            }),
            vec![Action::SetMax(2304 * MIB), Action::SetHigh(2048 * MIB)]
        );
        assert_eq!(engine.mode(), Mode::Steady);

        assert_eq!(
            wake(&mut engine, at(201), 2),
            vec![Action::SetHigh(2240 * MIB)]
        );
        assert_eq!(engine.mode(), Mode::Exhausted);
        assert!(engine.band_open());
    }

    #[test]
    fn kill_mode_never_retries_a_ceiling_deny() {
        let config = EngineConfig {
            exhaustion: ExhaustionMode::Kill,
            ..config()
        };
        let mut engine = titrated(config, 2560 * MIB);

        wake(&mut engine, at(100), 1);
        host(
            &mut engine,
            at(100),
            HostMessage::Deny {
                id: 1,
                reason: DenyReason::Ceiling,
                retry_after_ms: None,
            },
        );

        for second in 101..=200 {
            assert_eq!(
                tick(&mut engine, at(second), snap(BOOT, STALLED, 1)),
                vec![]
            );
        }

        assert_eq!(engine.mode(), Mode::Exhausted);
        assert_eq!(engine.pending_request(), None);
        assert!(engine.band_open());
    }

    #[test]
    fn a_target_already_at_the_ceiling_exhausts_without_asking() {
        let mut engine = titrated(config(), BOOT);

        assert_eq!(wake(&mut engine, at(100), 1), vec![Action::SetHigh(OPEN0)]);
        assert_eq!(engine.mode(), Mode::Exhausted);
        assert_eq!(engine.pending_request(), None);
    }

    #[test]
    fn a_stall_recovers_after_sustained_calm() {
        let mut engine = titrated(config(), BOOT);

        wake(&mut engine, at(100), 1);
        assert_eq!(engine.mode(), Mode::Exhausted);

        for second in 101..=104 {
            assert_eq!(
                tick(&mut engine, at(second), snap(BOOT, STALLED, 1)),
                vec![],
                "unexpected action at t+{second}"
            );
        }
        assert_eq!(engine.mode(), Mode::Exhausted);

        assert_eq!(tick(&mut engine, at(105), snap(BOOT, CALM, 1)), vec![]);
        assert_eq!(tick(&mut engine, at(106), snap(BOOT, CALM, 1)), vec![]);
        assert_eq!(engine.mode(), Mode::Exhausted);

        assert_eq!(
            tick(&mut engine, at(107), snap(BOOT, CALM, 1)),
            vec![Action::SetHigh(HIGH0)]
        );
        assert_eq!(engine.mode(), Mode::Steady);
        assert!(!engine.band_open());
    }

    #[test]
    fn recovery_needs_usage_back_under_the_steady_line() {
        let mut engine = titrated(config(), BOOT);

        wake(&mut engine, at(100), 1);

        for second in 101..=140 {
            let actions = tick(&mut engine, at(second), snap(BOOT, STALLED, 1));

            if second == 112 {
                assert_eq!(actions, vec![Action::SetHigh(MAX0)]);
            } else {
                assert_eq!(actions, vec![], "unexpected action at t+{second}");
            }
        }

        assert_eq!(engine.mode(), Mode::Exhausted);
        assert!(engine.band_open());
        assert_eq!(engine.high(), MAX0);
    }

    #[test]
    fn a_disconnected_stall_still_collapses() {
        let mut engine = titrated(config(), BOOT);

        wake(&mut engine, at(100), 1);
        engine.handle(Event::Disconnected { now: at(101) });
        assert_eq!(engine.mode(), Mode::Exhausted);
        assert_eq!(engine.high(), OPEN0);

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
    fn a_raised_ceiling_re_engages_the_guard_after_the_collapse() {
        let mut engine = titrated(config(), BOOT);

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
            vec![Action::SetHigh(OPEN0), request(1, BOOT, 2560 * MIB)]
        );
        assert_eq!(engine.mode(), Mode::AwaitingGrant);
        assert!(engine.high() < engine.max());
    }

    #[test]
    fn a_throttle_resets_the_recovery_count() {
        let mut engine = titrated(config(), BOOT);

        wake(&mut engine, at(100), 1);

        assert_eq!(tick(&mut engine, at(101), snap(BOOT, CALM, 1)), vec![]);
        assert_eq!(tick(&mut engine, at(102), snap(BOOT, CALM, 1)), vec![]);
        assert_eq!(tick(&mut engine, at(103), snap(BOOT, CALM, 2)), vec![]);
        assert_eq!(tick(&mut engine, at(104), snap(BOOT, CALM, 2)), vec![]);
        assert_eq!(tick(&mut engine, at(105), snap(BOOT, CALM, 2)), vec![]);
        assert_eq!(engine.mode(), Mode::Exhausted);

        assert_eq!(
            tick(&mut engine, at(106), snap(BOOT, CALM, 2)),
            vec![Action::SetHigh(HIGH0)]
        );
        assert_eq!(engine.mode(), Mode::Steady);
    }

    #[test]
    fn kill_mode_recovers_once_the_workload_is_calm() {
        let config = EngineConfig {
            exhaustion: ExhaustionMode::Kill,
            ..config()
        };
        let mut engine = titrated(config, 2560 * MIB);

        wake(&mut engine, at(100), 1);
        host(
            &mut engine,
            at(100),
            HostMessage::Deny {
                id: 1,
                reason: DenyReason::Ceiling,
                retry_after_ms: None,
            },
        );
        assert_eq!(engine.mode(), Mode::Exhausted);

        assert_eq!(tick(&mut engine, at(101), snap(BOOT, CALM, 1)), vec![]);

        assert_eq!(tick(&mut engine, at(102), snap(BOOT, CALM, 1)), vec![]);
        assert_eq!(tick(&mut engine, at(103), snap(BOOT, CALM, 1)), vec![]);
        assert_eq!(
            tick(&mut engine, at(104), snap(BOOT, CALM, 1)),
            vec![Action::SetHigh(HIGH0)]
        );
        assert_eq!(engine.mode(), Mode::Steady);
    }
}
