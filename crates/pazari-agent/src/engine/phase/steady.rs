use std::time::Duration;

use pazari_proto::{DenyReason, GuestMessage};

use crate::engine::{
    context::Context,
    limits::Band,
    phase::{Phase, State, growing::Growing, releasing::Releasing},
    time::Timestamp,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Steady;

impl Steady {
    pub fn from_titration(ctx: &mut Context) -> Phase {
        ctx.idle.reset();
        ctx.apply_limits(Band::Steady);

        Phase::Steady(Self)
    }

    pub fn settle(ctx: &mut Context, now: Timestamp) -> Phase {
        let cooldown = ctx.config.shrink_cooldown.get();

        ctx.idle.reset();
        ctx.idle.arm_cooldown(now, cooldown);
        ctx.apply_limits(Band::Steady);

        Phase::Steady(Self)
    }

    pub fn recover(ctx: &mut Context) -> Phase {
        ctx.apply_limits(Band::Steady);

        Phase::Steady(Self)
    }
}

impl State for Steady {
    fn advance(self, _ctx: &mut Context, _now: Timestamp) -> Phase {
        Phase::Steady(self)
    }

    fn brake(self, ctx: &mut Context, now: Timestamp) -> Phase {
        Growing::start(ctx, now, None)
    }

    fn on_plugged(self, ctx: &mut Context, _now: Timestamp, changed: bool) -> Phase {
        if changed {
            ctx.reapply_limits();
        }

        Phase::Steady(self)
    }

    fn on_grant(self, _ctx: &mut Context, _now: Timestamp, _id: u64, _target: u64) -> Phase {
        Phase::Steady(self)
    }

    fn on_deny(
        self,
        _ctx: &mut Context,
        _now: Timestamp,
        _id: u64,
        _reason: DenyReason,
        _retry_after: Option<Duration>,
    ) -> Phase {
        Phase::Steady(self)
    }

    fn on_reclaim_request(self, ctx: &mut Context, id: u64, desired: u64) -> Phase {
        let will_release = ctx.release_budget(desired);

        if will_release == 0 {
            ctx.nak(id, "no headroom to release without inducing pressure");

            return Phase::Steady(self);
        }

        ctx.send(GuestMessage::ReclaimAck { id, will_release });

        Releasing::begin(ctx, will_release)
    }

    fn on_reclaim_finished(self, _ctx: &mut Context, _now: Timestamp, _reclaimed: u64) -> Phase {
        Phase::Steady(self)
    }

    fn on_interval(self, ctx: &mut Context, now: Timestamp, throttled: bool) -> Phase {
        if !ctx.quiet_streak(throttled) {
            return Phase::Steady(self);
        }

        if !ctx.connected || ctx.idle.cooling(now) {
            return Phase::Steady(self);
        }

        let bytes = ctx.release_budget(ctx.config.shrink_step.get());

        if bytes == 0 {
            return Phase::Steady(self);
        }

        ctx.idle.reset();

        Releasing::begin(ctx, bytes)
    }

    fn on_ceiling_raised(self, _ctx: &mut Context, _now: Timestamp) -> Option<Phase> {
        None
    }

    fn on_connected(self) -> Phase {
        Phase::Steady(self)
    }

    fn on_disconnected(self, _ctx: &mut Context, _now: Timestamp) -> Phase {
        Phase::Steady(self)
    }

    fn resume(self, ctx: &mut Context, now: Timestamp) -> Phase {
        Growing::start(ctx, now, None)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pazari_proto::{GuestMessage, HostMessage};
    use pazari_types::Swappiness;

    use crate::engine::{Action, EngineConfig, Mode, testing::*};

    #[test]
    fn a_shrink_needs_exactly_the_configured_idle_intervals() {
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
        assert_eq!(engine.mode(), Mode::Releasing);
    }

    #[test]
    fn activity_resets_the_idle_counter() {
        let mut engine = started();
        grown(&mut engine);

        tick(&mut engine, at(140), snap(2560 * MIB, CALM, 1));
        tick(&mut engine, at(141), snap(2560 * MIB, CALM, 1));
        tick(&mut engine, at(142), snap(2560 * MIB, BUSY, 1));

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
    fn a_release_never_goes_below_boot_memory() {
        let mut engine = started();

        for second in 100..=110 {
            assert_eq!(
                tick(&mut engine, at(second), snap(BOOT, CALM, 0)),
                vec![],
                "unexpected action at t+{second}"
            );
        }

        assert_eq!(engine.mode(), Mode::Steady);
    }

    #[test]
    fn a_release_is_clamped_to_the_boot_memory_floor() {
        let config = EngineConfig {
            shrink_step: bytes(1024 * MIB),
            ..config()
        };
        let mut engine = titrated(config, CEILING);
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
        assert_eq!(engine.mode(), Mode::Releasing);
    }

    #[test]
    fn a_host_reclaim_request_is_naked_without_headroom() {
        let mut engine = started();

        tick(&mut engine, at(100), snap(BOOT, BUSY, 0));

        let actions = host(
            &mut engine,
            at(101),
            HostMessage::ReclaimRequest {
                id: 9,
                desired: 512 * MIB,
                deadline_ms: Duration::from_secs(30),
            },
        );

        assert!(matches!(
            actions.as_slice(),
            [Action::Send(GuestMessage::ReclaimNak { id: 9, .. })]
        ));
        assert_eq!(engine.mode(), Mode::Steady);
    }
}
