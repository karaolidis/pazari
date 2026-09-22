pub mod action;
pub mod config;
pub mod error;
pub mod event;

mod context;
mod counters;
mod idle;
mod limits;
mod phase;

#[cfg(test)]
pub(crate) mod testing;

use std::time::Duration;

use pazari_proto::HostMessage;
use pazari_types::Timestamp;

pub use crate::engine::{
    action::Action,
    config::{EngineConfig, ExhaustionMode, IdleThreshold},
    error::EngineError,
    event::{BootInfo, Event, Snapshot},
};
use crate::engine::{
    context::Context,
    limits::Band,
    phase::{Phase, State},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    Titrating,
    Steady,
    AwaitingGrant,
    Releasing,
    Exhausted,
}

#[derive(Debug, Clone)]
pub struct Engine {
    ctx: Context,
    phase: Phase,
}

impl Engine {
    pub fn try_new(
        config: EngineConfig,
        boot: BootInfo,
    ) -> Result<(Self, Vec<Action>), EngineError> {
        if boot.plugged < boot.boot_mem.get() {
            return Err(EngineError::PluggedBelowBoot {
                plugged: boot.plugged,
                boot_mem: boot.boot_mem.get(),
            });
        }

        let floor = config
            .service_reserve
            .saturating_add(config.burst_reserve.get())
            .saturating_add(boot.block_size.get());

        if boot.plugged < floor {
            return Err(EngineError::ReservesExceedMemory {
                plugged: boot.plugged,
                service_reserve: config.service_reserve,
                burst_reserve: config.burst_reserve.get(),
                block_size: boot.block_size.get(),
            });
        }

        if config.open_guard.get() >= config.burst_reserve.get() {
            return Err(EngineError::GuardSwallowsReserve {
                open_guard: config.open_guard.get(),
                burst_reserve: config.burst_reserve.get(),
            });
        }

        let oom_group = matches!(config.exhaustion, ExhaustionMode::Kill);

        let mut engine = Self {
            ctx: Context::new(config, boot),
            phase: Phase::start(),
        };

        let max = engine.ctx.limits.max();

        engine.ctx.emit(Action::SetMax(max));
        engine.ctx.emit(Action::SetHigh(max));
        engine.ctx.emit(Action::SetOomGroup(oom_group));

        let actions = engine.ctx.drain();

        Ok((engine, actions))
    }

    pub fn handle(&mut self, event: Event) -> Vec<Action> {
        match event {
            Event::Tick { now, snapshot } => self.on_tick(now, snapshot),
            Event::ThrottleWake {
                now,
                high_events,
                max_events,
            } => self.on_throttle_wake(now, high_events, max_events),
            Event::PluggedChanged { now, plugged } => self.on_plugged(now, plugged),
            Event::ReclaimFinished { now, reclaimed } => {
                self.phase = self
                    .phase
                    .on_reclaim_finished(&mut self.ctx, now, reclaimed);
            }
            Event::Connected {
                now,
                ceiling,
                status_interval,
            } => self.on_connected(now, ceiling, status_interval),
            Event::Disconnected { now } => self.on_disconnected(now),
            Event::Host { now, message } => self.on_host(now, &message),
        }

        self.ctx.drain()
    }

    #[must_use]
    pub const fn mode(&self) -> Mode {
        self.phase.mode()
    }

    #[must_use]
    pub const fn high(&self) -> u64 {
        self.ctx.limits.high()
    }

    #[must_use]
    pub const fn max(&self) -> u64 {
        self.ctx.limits.max()
    }

    #[must_use]
    pub const fn plugged(&self) -> u64 {
        self.ctx.limits.plugged()
    }

    #[must_use]
    pub const fn ceiling(&self) -> Option<u64> {
        self.ctx.limits.ceiling()
    }

    #[must_use]
    pub const fn is_connected(&self) -> bool {
        self.ctx.connected
    }

    #[must_use]
    pub const fn band_open(&self) -> bool {
        self.ctx.limits.is_open()
    }

    #[must_use]
    pub const fn pending_request(&self) -> Option<u64> {
        self.phase.pending_request()
    }

    fn on_tick(&mut self, now: Timestamp, snapshot: Snapshot) {
        let throttled = self.ctx.observe(snapshot);

        self.on_plugged(now, snapshot.plugged);

        if throttled {
            self.brake(now);
        }

        self.phase = self.phase.advance(&mut self.ctx, now);

        let interval_throttled = self.ctx.idle.take_throttled();
        self.phase = self
            .phase
            .on_interval(&mut self.ctx, now, interval_throttled);

        self.ctx.emit_status(now);
    }

    fn on_throttle_wake(&mut self, now: Timestamp, high_events: u64, max_events: u64) {
        if self.ctx.observe_wake(high_events, max_events) {
            self.brake(now);
        }
    }

    fn brake(&mut self, now: Timestamp) {
        self.ctx.idle.note_throttle();

        if self.ctx.limits.band() != Band::Collapsed {
            self.ctx.apply_limits(Band::Open);
        }

        self.phase = self.phase.brake(&mut self.ctx, now);
    }

    fn on_plugged(&mut self, now: Timestamp, plugged: u64) {
        let changed = self.ctx.limits.set_plugged(plugged);

        self.phase = self.phase.on_plugged(&mut self.ctx, now, changed);
    }

    fn on_connected(&mut self, now: Timestamp, ceiling: u64, status_interval: Duration) {
        self.ctx.connect(status_interval);
        self.phase = self.phase.on_connected();
        self.ctx.emit_status(now);

        if !self.adopt_ceiling(now, ceiling) && self.ctx.limits.band() == Band::Open {
            self.phase = self.phase.resume(&mut self.ctx, now);
        }
    }

    fn on_disconnected(&mut self, now: Timestamp) {
        self.ctx.disconnect();

        self.phase = self.phase.on_disconnected(&mut self.ctx, now);
    }

    fn on_host(&mut self, now: Timestamp, message: &HostMessage) {
        if !self.ctx.connected {
            return;
        }

        match *message {
            HostMessage::Welcome { .. } => {}
            HostMessage::Ceiling { ceiling } => {
                self.adopt_ceiling(now, ceiling);
            }
            HostMessage::Grant { id, target } => {
                self.phase = self.phase.on_grant(&mut self.ctx, now, id, target);
            }
            HostMessage::Deny {
                id,
                reason,
                retry_after_ms,
            } => {
                self.phase = self
                    .phase
                    .on_deny(&mut self.ctx, now, id, reason, retry_after_ms);
            }
            HostMessage::ReclaimRequest { id, desired, .. } => {
                self.phase = self.phase.on_reclaim_request(&mut self.ctx, id, desired);
            }
        }
    }

    fn adopt_ceiling(&mut self, now: Timestamp, ceiling: u64) -> bool {
        if !self.ctx.limits.raise_ceiling(ceiling) {
            return false;
        }

        let Some(phase) = self.phase.on_ceiling_raised(&mut self.ctx, now) else {
            return false;
        };

        self.phase = phase;

        true
    }
}

#[cfg(test)]
mod tests {
    use pazari_proto::{DenyReason, GuestMessage, HostMessage, Status};
    use proptest::prelude::*;

    use super::*;
    use crate::engine::testing::*;

    fn properties_config() -> EngineConfig {
        EngineConfig {
            exhaustion: ExhaustionMode::Stall {
                timeout: interval(60),
            },
            ..config()
        }
    }

    #[test]
    fn new_establishes_ground_state() {
        let (engine, actions) = Engine::try_new(config(), boot()).expect("a workable boot");

        assert_eq!(
            actions,
            vec![
                Action::SetMax(MAX0),
                Action::SetHigh(MAX0),
                Action::SetOomGroup(false),
            ]
        );
        assert_eq!(engine.mode(), Mode::Titrating);
        assert_eq!(engine.high(), engine.max());
        assert_eq!(engine.ceiling(), None);
    }

    #[test]
    fn kill_mode_arms_the_oom_group() {
        let config = EngineConfig {
            exhaustion: ExhaustionMode::Kill,
            ..config()
        };
        let (_, actions) = Engine::try_new(config, boot()).expect("a workable boot");

        assert_eq!(actions[2], Action::SetOomGroup(true));
    }

    #[test]
    fn the_brake_releases_the_band_before_it_asks() {
        let mut engine = started();

        let actions = wake(&mut engine, at(100), 1);

        assert_eq!(
            actions,
            vec![Action::SetHigh(OPEN0), request(1, BOOT, 2560 * MIB)]
        );
        assert_eq!(engine.pending_request(), Some(1));
        assert!(engine.high() < engine.max());
    }

    #[test]
    fn status_follows_the_interval_learned_at_connect() {
        let (mut engine, _) = Engine::try_new(config(), boot()).expect("a workable boot");

        let actions = connect(&mut engine, at(0), CEILING, Duration::from_secs(5));
        assert!(matches!(
            actions.as_slice(),
            [Action::Send(GuestMessage::Status(..))]
        ));

        assert_eq!(tick(&mut engine, at(4), snap(BOOT, BUSY, 0)), vec![]);

        assert_eq!(
            tick(&mut engine, at(5), snap(BOOT, 1234 * MIB, 0)),
            vec![Action::Send(GuestMessage::Status(Status {
                plugged: BOOT,
                available: BOOT - 1234 * MIB,
                high_events: 0,
                max_events: 0,
                psi_some10: 0.0,
                psi_full10: 0.0,
                swap_used: 0,
                workload_usage: 1234 * MIB,
            }))]
        );
    }

    #[test]
    fn status_carries_the_counters_the_wake_reported() {
        let (mut engine, _) = Engine::try_new(config(), boot()).expect("a workable boot");

        connect(&mut engine, at(0), CEILING, HOUR);
        tick(&mut engine, at(1), snap(BOOT, BUSY, 0));

        engine.handle(Event::ThrottleWake {
            now: at(2),
            high_events: 9,
            max_events: 3,
        });
        engine.handle(Event::Disconnected { now: at(3) });

        let actions = connect(&mut engine, at(4), CEILING, HOUR);

        assert_eq!(
            actions[0],
            Action::Send(GuestMessage::Status(Status {
                plugged: BOOT,
                available: BOOT - BUSY,
                high_events: 9,
                max_events: 3,
                psi_some10: 0.0,
                psi_full10: 0.0,
                swap_used: 0,
                workload_usage: BUSY,
            }))
        );
    }

    #[test]
    fn nothing_is_sent_while_disconnected() {
        let mut engine = started();

        engine.handle(Event::Disconnected { now: at(100) });

        for second in 101..=140 {
            let actions = tick(&mut engine, at(second), snap(BOOT, CALM, 0));
            assert!(
                !actions
                    .iter()
                    .any(|action| matches!(action, Action::Send(_))),
                "sent while disconnected at t+{second}"
            );
        }
    }

    #[test]
    fn the_brake_still_works_while_disconnected() {
        let mut engine = started();

        engine.handle(Event::Disconnected { now: at(100) });

        assert_eq!(wake(&mut engine, at(101), 1), vec![Action::SetHigh(OPEN0)]);
        assert_eq!(engine.mode(), Mode::AwaitingGrant);
        assert_eq!(engine.pending_request(), None);
        assert!(!engine.is_connected());
    }

    #[test]
    fn reconnecting_restarts_ids_and_re_issues_the_request() {
        let mut engine = started();

        wake(&mut engine, at(100), 1);
        assert_eq!(engine.pending_request(), Some(1));

        engine.handle(Event::Disconnected { now: at(101) });
        assert_eq!(engine.pending_request(), None);

        let actions = connect(&mut engine, at(102), CEILING, HOUR);

        assert!(matches!(
            actions.as_slice(),
            [Action::Send(GuestMessage::Status(..)), _]
        ));
        assert_eq!(actions[1], request(1, BOOT, 2560 * MIB));
        assert_eq!(engine.pending_request(), Some(1));
    }

    #[test]
    fn a_reconnect_without_an_unserved_throttle_only_sends_status() {
        let mut engine = started();

        engine.handle(Event::Disconnected { now: at(100) });
        let actions = connect(&mut engine, at(101), CEILING, HOUR);

        assert!(matches!(
            actions.as_slice(),
            [Action::Send(GuestMessage::Status(..))]
        ));
        assert_eq!(engine.mode(), Mode::Steady);
    }

    #[test]
    fn the_first_counter_observation_is_not_a_throttle() {
        let (mut engine, _) = Engine::try_new(config(), boot()).expect("a workable boot");

        assert_eq!(tick(&mut engine, at(0), snap(BOOT, BUSY, 5)), vec![]);
        assert_eq!(engine.mode(), Mode::Titrating);
        assert_eq!(engine.high(), MAX0);
    }

    #[test]
    fn only_an_advancing_counter_is_a_throttle() {
        let mut engine = started();

        assert_eq!(tick(&mut engine, at(100), snap(BOOT, BUSY, 0)), vec![]);
        assert_eq!(tick(&mut engine, at(101), snap(BOOT, BUSY, 0)), vec![]);

        assert_eq!(
            tick(&mut engine, at(102), snap(BOOT, BUSY, 2)),
            vec![Action::SetHigh(OPEN0), request(1, BOOT, 2560 * MIB)]
        );
    }

    #[test]
    fn a_counter_reset_is_not_a_throttle() {
        let (mut engine, _) = Engine::try_new(config(), boot()).expect("a workable boot");

        assert_eq!(tick(&mut engine, at(0), snap(BOOT, BUSY, 7)), vec![]);
        assert_eq!(tick(&mut engine, at(1), snap(BOOT, BUSY, 2)), vec![]);

        assert_eq!(engine.mode(), Mode::Titrating);
        assert_eq!(engine.high(), MAX0);
    }

    #[test]
    fn a_spurious_wake_without_a_counter_advance_is_ignored() {
        let mut engine = started();

        tick(&mut engine, at(100), snap(BOOT, BUSY, 0));

        assert_eq!(wake(&mut engine, at(101), 0), vec![]);
        assert_eq!(engine.mode(), Mode::Steady);
    }

    #[test]
    fn limits_track_plugged_when_it_moves_underneath_steady_state() {
        let mut engine = started();

        assert_eq!(
            engine.handle(Event::PluggedChanged {
                now: at(100),
                plugged: 2560 * MIB,
            }),
            vec![Action::SetMax(2304 * MIB), Action::SetHigh(2048 * MIB)]
        );

        assert_eq!(
            engine.handle(Event::PluggedChanged {
                now: at(101),
                plugged: BOOT,
            }),
            vec![Action::SetHigh(HIGH0), Action::SetMax(MAX0)]
        );
    }

    #[test]
    fn a_guest_smaller_than_its_reserves_is_rejected() {
        let boot = BootInfo {
            boot_mem: bytes(128 * MIB),
            block_size: bytes(BLOCK),
            plugged: 128 * MIB,
        };

        assert_eq!(
            Engine::try_new(config(), boot).expect_err("the reserves swallow the guest"),
            EngineError::ReservesExceedMemory {
                plugged: 128 * MIB,
                service_reserve: SERVICE,
                burst_reserve: BURST,
                block_size: BLOCK,
            }
        );
    }

    #[test]
    fn a_guest_without_a_block_to_spare_is_rejected() {
        let boot = BootInfo {
            boot_mem: bytes(128 * MIB),
            block_size: bytes(BLOCK),
            plugged: SERVICE + BURST + BLOCK - 1,
        };

        assert_eq!(
            Engine::try_new(config(), boot)
                .expect_err("the reserves leave less than a block to work in"),
            EngineError::ReservesExceedMemory {
                plugged: SERVICE + BURST + BLOCK - 1,
                service_reserve: SERVICE,
                burst_reserve: BURST,
                block_size: BLOCK,
            }
        );
    }

    #[test]
    fn a_guest_with_exactly_a_block_to_spare_boots() {
        let boot = BootInfo {
            boot_mem: bytes(128 * MIB),
            block_size: bytes(BLOCK),
            plugged: SERVICE + BURST + BLOCK,
        };

        let (engine, _) = Engine::try_new(config(), boot).expect("a block is workable");

        assert_eq!(engine.max(), BURST + BLOCK);
    }

    #[test]
    fn plugged_below_boot_memory_is_rejected() {
        let boot = BootInfo {
            boot_mem: bytes(BOOT),
            block_size: bytes(BLOCK),
            plugged: BOOT - BLOCK,
        };

        assert_eq!(
            Engine::try_new(config(), boot).expect_err("a prober reporting less than boot memory"),
            EngineError::PluggedBelowBoot {
                plugged: BOOT - BLOCK,
                boot_mem: BOOT,
            }
        );
    }

    #[test]
    fn a_guard_no_smaller_than_the_reserve_is_rejected() {
        let config = EngineConfig {
            open_guard: bytes(BURST),
            ..config()
        };

        assert_eq!(
            Engine::try_new(config, boot()).expect_err("the guard swallows the reserve"),
            EngineError::GuardSwallowsReserve {
                open_guard: BURST,
                burst_reserve: BURST,
            }
        );
    }

    #[derive(Debug, Clone, Copy)]
    enum Step {
        Tick {
            plugged: u64,
            usage: u64,
            available: u64,
            high_events: u64,
            psi: f32,
        },
        Wake {
            high_events: u64,
        },
        Plug {
            plugged: u64,
        },
        Reclaimed {
            reclaimed: u64,
        },
        Connect {
            ceiling: u64,
        },
        Disconnect,
        Ceiling {
            ceiling: u64,
        },
        Answer {
            grant: bool,
            target: u64,
            reason: DenyReason,
        },
        Reclaim {
            id: u64,
            desired: u64,
        },
    }

    fn byte_count() -> impl Strategy<Value = u64> {
        prop_oneof![
            1 => Just(0_u64),
            1 => Just(u64::MAX),
            8 => (0_u64..8192).prop_map(|mib| mib * MIB),
        ]
    }

    fn deny_reason() -> impl Strategy<Value = DenyReason> {
        prop_oneof![
            Just(DenyReason::Ceiling),
            Just(DenyReason::HostPressure),
            Just(DenyReason::RateLimited),
            Just(DenyReason::Policy),
        ]
    }

    fn step() -> impl Strategy<Value = Step> {
        prop_oneof![
            8 => (byte_count(), byte_count(), byte_count(), 0_u64..8, 0.0_f32..100.0).prop_map(
                |(plugged, usage, available, high_events, psi)| Step::Tick {
                    plugged,
                    usage,
                    available,
                    high_events,
                    psi,
                }
            ),
            4 => (0_u64..8).prop_map(|high_events| Step::Wake { high_events }),
            3 => byte_count().prop_map(|plugged| Step::Plug { plugged }),
            2 => byte_count().prop_map(|reclaimed| Step::Reclaimed { reclaimed }),
            2 => byte_count().prop_map(|ceiling| Step::Connect { ceiling }),
            1 => Just(Step::Disconnect),
            2 => byte_count().prop_map(|ceiling| Step::Ceiling { ceiling }),
            4 => (any::<bool>(), byte_count(), deny_reason()).prop_map(|(grant, target, reason)| {
                Step::Answer {
                    grant,
                    target,
                    reason,
                }
            }),
            2 => (0_u64..4, byte_count()).prop_map(|(id, desired)| Step::Reclaim { id, desired }),
        ]
    }

    fn timeline() -> impl Strategy<Value = Vec<(u64, Step)>> {
        prop::collection::vec((0_u64..5_000, step()), 0..48)
    }

    struct Model {
        connected: bool,
        last_id: u64,
    }

    fn check(engine: &Engine, actions: &[Action], model: &mut Model) -> Result<(), TestCaseError> {
        prop_assert!(
            engine.high() <= engine.max(),
            "high {} over max {}",
            engine.high(),
            engine.max()
        );

        if engine.pending_request().is_some() {
            prop_assert!(
                engine.high() < engine.max() || engine.max() == 0,
                "a pending request with no brake line: high {} max {}",
                engine.high(),
                engine.max()
            );
        }

        for action in actions {
            let Action::Send(message) = action else {
                continue;
            };

            prop_assert!(model.connected, "sent {message:?} while disconnected");

            let (GuestMessage::GrantRequest { id, .. } | GuestMessage::OfferRelease { id, .. }) =
                message
            else {
                continue;
            };

            prop_assert!(
                *id > model.last_id,
                "id {id} does not follow {}",
                model.last_id
            );
            model.last_id = *id;
        }

        if engine.mode() == Mode::Steady && engine.pending_request().is_none() {
            let max = engine.plugged().saturating_sub(SERVICE);

            prop_assert_eq!(engine.max(), max, "steady max off formula");
            prop_assert_eq!(
                engine.high(),
                max.saturating_sub(BURST),
                "steady high off formula"
            );
        }

        Ok(())
    }

    fn drive(timeline: &[(u64, Step)]) -> Result<(), TestCaseError> {
        let (mut engine, actions) =
            Engine::try_new(properties_config(), boot()).expect("a workable boot");
        let mut now = Timestamp::START;
        let mut model = Model {
            connected: false,
            last_id: 0,
        };

        check(&engine, &actions, &mut model)?;

        for (gap, step) in timeline {
            now = now.saturating_add(Duration::from_millis(*gap));

            let event = match *step {
                Step::Tick {
                    plugged,
                    usage,
                    available,
                    high_events,
                    psi,
                } => Event::Tick {
                    now,
                    snapshot: Snapshot {
                        plugged,
                        workload_usage: usage,
                        available,
                        swap_used: 0,
                        high_events,
                        max_events: 0,
                        psi_some10: psi,
                        psi_full10: psi,
                    },
                },
                Step::Wake { high_events } => Event::ThrottleWake {
                    now,
                    high_events,
                    max_events: 0,
                },
                Step::Plug { plugged } => Event::PluggedChanged { now, plugged },
                Step::Reclaimed { reclaimed } => Event::ReclaimFinished { now, reclaimed },
                Step::Connect { ceiling } => {
                    model.connected = true;
                    model.last_id = 0;

                    Event::Connected {
                        now,
                        ceiling,
                        status_interval: Duration::from_millis(500),
                    }
                }
                Step::Disconnect => {
                    model.connected = false;
                    Event::Disconnected { now }
                }
                Step::Ceiling { ceiling } => Event::Host {
                    now,
                    message: HostMessage::Ceiling { ceiling },
                },
                Step::Answer {
                    grant,
                    target,
                    reason,
                } => {
                    let id = engine.pending_request().unwrap_or(0);
                    let message = if grant {
                        HostMessage::Grant { id, target }
                    } else {
                        HostMessage::Deny {
                            id,
                            reason,
                            retry_after_ms: None,
                        }
                    };

                    Event::Host { now, message }
                }
                Step::Reclaim { id, desired } => Event::Host {
                    now,
                    message: HostMessage::ReclaimRequest {
                        id,
                        desired,
                        deadline_ms: Duration::from_secs(30),
                    },
                },
            };

            let actions = engine.handle(event);
            check(&engine, &actions, &mut model)?;
        }

        Ok(())
    }

    proptest! {
        #[test]
        fn the_engine_holds_its_invariants_over_arbitrary_event_streams(timeline in timeline()) {
            drive(&timeline)?;
        }
    }
}
