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

use std::{fmt, time::Duration};

use pazari_proto::HostMessage;
use pazari_types::Timestamp;
use tracing::debug;

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

impl Mode {
    pub const ALL: [Self; 5] = [
        Self::Titrating,
        Self::Steady,
        Self::AwaitingGrant,
        Self::Releasing,
        Self::Exhausted,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Titrating => "titrating",
            Self::Steady => "steady",
            Self::AwaitingGrant => "awaiting_grant",
            Self::Releasing => "releasing",
            Self::Exhausted => "exhausted",
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
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
        let floor = config
            .service_reserve
            .saturating_add(config.burst_reserve.get())
            .saturating_add(boot.block_size.get());

        if boot.total_mem < floor {
            return Err(EngineError::ReservesExceedMemory {
                total_mem: boot.total_mem,
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
        let before = self.phase.mode();

        self.dispatch(event);

        let mode = self.phase.mode();

        if mode != before {
            debug!(
                %mode,
                from = %before,
                high = self.ctx.limits.high(),
                max = self.ctx.limits.max(),
                total_mem = self.ctx.limits.total_mem(),
                usage = self.ctx.last.workload_usage,
                "the engine changed phase",
            );
        }

        self.ctx.drain()
    }

    fn dispatch(&mut self, event: Event) {
        match event {
            Event::Tick { now, snapshot } => self.on_tick(now, snapshot),
            Event::ThrottleWake {
                now,
                high_events,
                max_events,
            } => self.on_throttle_wake(now, high_events, max_events),
            Event::TotalChanged { now, total_mem } => self.on_total_changed(now, total_mem),
            Event::ReclaimFinished { now, reclaimed } => {
                self.phase = self
                    .phase
                    .on_reclaim_finished(&mut self.ctx, now, reclaimed);
            }
            Event::Connected {
                now,
                ceiling,
                status_interval,
                hotplugged,
            } => self.on_connected(now, ceiling, status_interval, hotplugged),
            Event::Disconnected { now } => self.on_disconnected(now),
            Event::Host { now, message } => self.on_host(now, &message),
        }
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
    pub const fn total_mem(&self) -> u64 {
        self.ctx.limits.total_mem()
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

        self.on_total_changed(now, snapshot.total_mem);

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

        let collapsed = self.ctx.limits.band() == Band::Collapsed;

        debug!(
            usage = self.ctx.last.workload_usage,
            high = self.ctx.limits.high(),
            total_mem = self.ctx.limits.total_mem(),
            collapsed,
            "workload throttled at memory.high",
        );

        if !collapsed {
            self.ctx.apply_limits(Band::Open);
        }

        self.phase = self.phase.brake(&mut self.ctx, now);
    }

    fn on_total_changed(&mut self, now: Timestamp, total_mem: u64) {
        let changed = self.ctx.limits.set_total_mem(total_mem);

        self.phase = self.phase.on_total_changed(&mut self.ctx, now, changed);
    }

    fn on_connected(
        &mut self,
        now: Timestamp,
        ceiling: u64,
        status_interval: Duration,
        hotplugged: u64,
    ) {
        self.ctx.connect(status_interval, hotplugged);
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
            HostMessage::Geometry { hotplugged } => self.ctx.set_hotplugged(hotplugged),
            HostMessage::Grant { id, target } => {
                debug!(id, target, "host granted the request");

                self.phase = self.phase.on_grant(&mut self.ctx, now, id, target);
            }
            HostMessage::Deny {
                id,
                reason,
                retry_after_ms,
            } => {
                debug!(
                    id,
                    ?reason,
                    retry_after = ?retry_after_ms,
                    "host denied the request",
                );

                self.phase = self
                    .phase
                    .on_deny(&mut self.ctx, now, id, reason, retry_after_ms);
            }
            HostMessage::ReclaimRequest { id, desired, .. } => {
                debug!(id, desired, "host requested memory back");

                self.phase = self.phase.on_reclaim_request(&mut self.ctx, id, desired);
            }
        }
    }

    fn adopt_ceiling(&mut self, now: Timestamp, ceiling: u64) -> bool {
        if !self.ctx.limits.raise_ceiling(ceiling) {
            return false;
        }

        debug!(
            ceiling,
            total_mem = self.ctx.limits.total_mem(),
            "ceiling raised",
        );

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
                total_mem: BOOT,
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
                total_mem: BOOT,
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
    fn limits_track_the_total_when_it_moves_underneath_steady_state() {
        let mut engine = started();

        assert_eq!(
            engine.handle(Event::TotalChanged {
                now: at(100),
                total_mem: 2560 * MIB,
            }),
            vec![Action::SetMax(2304 * MIB), Action::SetHigh(2048 * MIB)]
        );

        assert_eq!(
            engine.handle(Event::TotalChanged {
                now: at(101),
                total_mem: BOOT,
            }),
            vec![Action::SetHigh(HIGH0), Action::SetMax(MAX0)]
        );
    }

    #[test]
    fn a_guest_smaller_than_its_reserves_is_rejected() {
        let boot = BootInfo {
            block_size: bytes(BLOCK),
            total_mem: 128 * MIB,
        };

        assert_eq!(
            Engine::try_new(config(), boot).expect_err("the reserves swallow the guest"),
            EngineError::ReservesExceedMemory {
                total_mem: 128 * MIB,
                service_reserve: SERVICE,
                burst_reserve: BURST,
                block_size: BLOCK,
            }
        );
    }

    #[test]
    fn a_guest_without_a_block_to_spare_is_rejected() {
        let boot = BootInfo {
            block_size: bytes(BLOCK),
            total_mem: SERVICE + BURST + BLOCK - 1,
        };

        assert_eq!(
            Engine::try_new(config(), boot)
                .expect_err("the reserves leave less than a block to work in"),
            EngineError::ReservesExceedMemory {
                total_mem: SERVICE + BURST + BLOCK - 1,
                service_reserve: SERVICE,
                burst_reserve: BURST,
                block_size: BLOCK,
            }
        );
    }

    #[test]
    fn a_guest_with_exactly_a_block_to_spare_boots() {
        let boot = BootInfo {
            block_size: bytes(BLOCK),
            total_mem: SERVICE + BURST + BLOCK,
        };

        let (engine, _) = Engine::try_new(config(), boot).expect("a block is workable");

        assert_eq!(engine.max(), BURST + BLOCK);
    }

    #[test]
    fn boot_memory_the_guest_cannot_see_is_held_at_what_it_can() {
        let boot = BootInfo {
            block_size: bytes(BLOCK),
            total_mem: BOOT - BLOCK,
        };

        let (engine, _) = Engine::try_new(config(), boot).expect("a workable boot");

        assert_eq!(engine.total_mem(), BOOT - BLOCK);
        assert_eq!(engine.ctx.release_budget(u64::MAX), 0);
    }

    #[test]
    fn only_the_host_can_say_how_much_of_the_guest_is_hot_plugged() {
        let boot = BootInfo {
            block_size: bytes(BLOCK),
            total_mem: BOOT + 4 * BLOCK,
        };

        let (mut engine, _) = Engine::try_new(config(), boot).expect("a workable boot");

        assert_eq!(engine.ctx.release_budget(u64::MAX), 0);

        reconnect(&mut engine, at(0), CEILING, 4 * BLOCK);

        assert_eq!(engine.ctx.release_budget(u64::MAX), 4 * BLOCK);
    }

    #[test]
    fn a_floor_taken_before_a_plug_lands_corrects_itself_when_it_does() {
        let (mut engine, _) = Engine::try_new(config(), boot()).expect("a workable boot");

        connect(&mut engine, at(0), CEILING, HOUR);

        assert_eq!(engine.ctx.release_budget(u64::MAX), 0);

        host(
            &mut engine,
            at(1),
            HostMessage::Geometry {
                hotplugged: 4 * BLOCK,
            },
        );

        engine.handle(Event::TotalChanged {
            now: at(2),
            total_mem: BOOT + 4 * BLOCK,
        });

        assert_eq!(
            engine.total_mem() - engine.ctx.release_budget(u64::MAX),
            BOOT,
            "the guest would have offered past the memory it booted with",
        );
    }

    #[test]
    fn a_guest_offers_back_no_more_than_the_host_says_is_plugged_in() {
        let boot = BootInfo {
            block_size: bytes(BLOCK),
            total_mem: BOOT + 4 * BLOCK,
        };

        let (mut engine, _) = Engine::try_new(config(), boot).expect("a workable boot");

        reconnect(&mut engine, at(0), CEILING, 4 * BLOCK);

        assert_eq!(engine.ctx.release_budget(u64::MAX), 4 * BLOCK);

        host(
            &mut engine,
            at(1),
            HostMessage::Geometry {
                hotplugged: 2 * BLOCK,
            },
        );

        assert_eq!(engine.ctx.release_budget(u64::MAX), 2 * BLOCK);

        engine.handle(Event::TotalChanged {
            now: at(2),
            total_mem: BOOT + 2 * BLOCK,
        });

        assert_eq!(engine.ctx.release_budget(u64::MAX), 2 * BLOCK);
    }

    #[test]
    fn a_reconnect_takes_the_floor_the_host_gives_it_that_time() {
        let boot = BootInfo {
            block_size: bytes(BLOCK),
            total_mem: BOOT + 4 * BLOCK,
        };

        let (mut engine, _) = Engine::try_new(config(), boot).expect("a workable boot");

        reconnect(&mut engine, at(0), CEILING, 4 * BLOCK);
        engine.handle(Event::Disconnected { now: at(1) });

        engine.handle(Event::TotalChanged {
            now: at(2),
            total_mem: BOOT + 2 * BLOCK,
        });
        reconnect(&mut engine, at(3), CEILING, 2 * BLOCK);

        assert_eq!(engine.ctx.release_budget(u64::MAX), 2 * BLOCK);
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
            total_mem: u64,
            usage: u64,
            available: u64,
            high_events: u64,
            psi: f32,
        },
        Wake {
            high_events: u64,
        },
        Plug {
            total_mem: u64,
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
                |(total_mem, usage, available, high_events, psi)| Step::Tick {
                    total_mem,
                    usage,
                    available,
                    high_events,
                    psi,
                }
            ),
            4 => (0_u64..8).prop_map(|high_events| Step::Wake { high_events }),
            3 => byte_count().prop_map(|total_mem| Step::Plug { total_mem }),
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
            let max = engine.total_mem().saturating_sub(SERVICE);

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
                    total_mem,
                    usage,
                    available,
                    high_events,
                    psi,
                } => Event::Tick {
                    now,
                    snapshot: Snapshot {
                        total_mem,
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
                Step::Plug { total_mem } => Event::TotalChanged { now, total_mem },
                Step::Reclaimed { reclaimed } => Event::ReclaimFinished { now, reclaimed },
                Step::Connect { ceiling } => {
                    model.connected = true;
                    model.last_id = 0;

                    Event::Connected {
                        now,
                        ceiling,
                        status_interval: Duration::from_millis(500),
                        hotplugged: 0,
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
