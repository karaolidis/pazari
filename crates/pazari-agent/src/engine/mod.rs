pub mod action;
pub mod config;
pub mod error;
pub mod event;

mod counters;
mod handle;

mod effects;
mod grow;
mod limits;
mod phase;
mod release;
mod streak;
mod world;

#[cfg(test)]
pub(crate) mod testing;

use pazari_types::Timestamp;
use tracing::debug;

pub use crate::engine::{
    action::Action,
    config::{
        EngineConfig, ExhaustionMode, ExponentialConfig, GrowConfig, ReleaseConfig, TitrateConfig,
    },
    error::EngineError,
    event::{BootInfo, Event, Snapshot},
    phase::Phase,
};
use crate::engine::{
    counters::CounterTracker,
    effects::Effects,
    phase::{State, Step},
    world::World,
};

#[derive(Debug, Clone)]
pub struct Engine {
    world: World,
    counters: CounterTracker,
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

        let engine = Self {
            world: World::new(config, boot),
            counters: CounterTracker::default(),
            phase: Phase::start(),
        };

        let limits = engine.world.limits();

        let actions = vec![
            Action::SetMax(limits.max()),
            Action::SetHigh(limits.high()),
            Action::SetOomGroup(matches!(config.exhaustion, ExhaustionMode::Kill)),
        ];

        Ok((engine, actions))
    }

    pub fn handle(&mut self, event: Event) -> Vec<Action> {
        let before = self.phase;
        let mut fx = Effects::default();

        self.dispatch(event, &mut fx);

        if self.phase.name() != before.name() {
            debug!(
                phase = %self.phase,
                from = %before,
                brake = ?self.world.limits().brake(),
                high = self.world.limits().high(),
                max = self.world.limits().max(),
                total_mem = self.world.limits().total_mem(),
                available = ?self.world.guest().available(),
                slack = ?self.world.slack(),
                usage = self.world.guest().usage(),
                "the engine changed phase",
            );
        }

        fx.drain()
    }

    fn dispatch(&mut self, event: Event, fx: &mut Effects) {
        match event {
            Event::Tick { now, snapshot } => self.on_tick(now, snapshot, fx),
            Event::ThrottleWake {
                now,
                high_events,
                max_events,
            } => self.on_throttle_wake(now, high_events, max_events, fx),
            Event::TotalChanged { now, total_mem } => self.on_total_changed(now, total_mem, fx),
            Event::ReclaimFinished { now, reclaimed } => {
                self.transition(now, fx, |phase, step| {
                    phase.on_reclaim_finished(step, reclaimed)
                });
            }
            Event::Connected {
                now,
                ceiling,
                status_interval,
                hotplugged,
            } => self.on_connected(now, ceiling, status_interval, hotplugged, fx),
            Event::Disconnected { now } => self.on_disconnected(now, fx),
            Event::Host { now, message } => self.on_host(now, &message, fx),
        }
    }

    fn transition(
        &mut self,
        now: Timestamp,
        fx: &mut Effects,
        go: impl FnOnce(Phase, &mut Step<'_>) -> Phase,
    ) {
        let mut step = Step {
            world: &mut self.world,
            fx,
            now,
        };

        self.phase = go(self.phase, &mut step);
    }

    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    #[must_use]
    pub const fn high(&self) -> u64 {
        self.world.limits().high()
    }

    #[must_use]
    pub const fn max(&self) -> u64 {
        self.world.limits().max()
    }

    #[must_use]
    pub const fn total_mem(&self) -> u64 {
        self.world.limits().total_mem()
    }

    #[must_use]
    pub const fn ceiling(&self) -> Option<u64> {
        self.world.limits().ceiling()
    }

    #[must_use]
    pub const fn is_connected(&self) -> bool {
        self.world.connected()
    }

    #[must_use]
    pub const fn brake_released(&self) -> bool {
        self.world.limits().released()
    }

    #[must_use]
    pub const fn pending_request(&self) -> Option<u64> {
        self.phase.pending_request()
    }
}
#[cfg(test)]
mod tests {
    use pazari_proto::{DenyReason, GuestMessage, HostMessage, Status};
    use proptest::prelude::*;
    use std::time::Duration;

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
                Action::SetHigh(HIGH0),
                Action::SetOomGroup(false),
            ]
        );
        assert!(matches!(engine.phase(), Phase::Steady(_)));
        assert_eq!(engine.high(), HIGH0);
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
    fn the_brake_asks_without_letting_the_workload_further_up() {
        let mut engine = started();

        let actions = wake(&mut engine, at(100), 1);

        assert_eq!(actions, vec![request(1, BOOT, 2560 * MIB)]);
        assert_eq!(engine.pending_request(), Some(1));
        assert_eq!(engine.high(), HIGH0);
        assert_eq!(engine.max(), MAX0);
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

        assert_eq!(wake(&mut engine, at(101), 1), vec![]);
        assert!(matches!(engine.phase(), Phase::Growing(_)));
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
        assert!(matches!(engine.phase(), Phase::Steady(_)));
    }

    #[test]
    fn the_first_counter_observation_is_not_a_throttle() {
        let (mut engine, _) = Engine::try_new(config(), boot()).expect("a workable boot");

        assert_eq!(tick(&mut engine, at(0), snap(BOOT, BUSY, 5)), vec![]);
        assert!(matches!(engine.phase(), Phase::Steady(_)));
        assert_eq!(engine.high(), HIGH0);
    }

    #[test]
    fn only_an_advancing_counter_is_a_throttle() {
        let mut engine = started();

        assert_eq!(tick(&mut engine, at(100), snap(BOOT, BUSY, 0)), vec![]);
        assert_eq!(tick(&mut engine, at(101), snap(BOOT, BUSY, 0)), vec![]);

        assert_eq!(
            tick(&mut engine, at(102), snap(BOOT, BUSY, 2)),
            vec![request(1, BOOT, 2560 * MIB)]
        );
    }

    #[test]
    fn a_counter_reset_is_not_a_throttle() {
        let (mut engine, _) = Engine::try_new(config(), boot()).expect("a workable boot");

        assert_eq!(tick(&mut engine, at(0), snap(BOOT, BUSY, 7)), vec![]);
        assert_eq!(tick(&mut engine, at(1), snap(BOOT, BUSY, 2)), vec![]);

        assert!(matches!(engine.phase(), Phase::Steady(_)));
        assert_eq!(engine.high(), HIGH0);
    }

    #[test]
    fn a_spurious_wake_without_a_counter_advance_is_ignored() {
        let mut engine = started();

        tick(&mut engine, at(100), snap(BOOT, BUSY, 0));

        assert_eq!(wake(&mut engine, at(101), 0), vec![]);
        assert!(matches!(engine.phase(), Phase::Steady(_)));
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

        let (mut engine, _) = Engine::try_new(config(), boot).expect("a workable boot");
        engine.world.observe(snap(BOOT - BLOCK, 0, 0));

        assert_eq!(engine.total_mem(), BOOT - BLOCK);
        assert_eq!(engine.world.release_budget(u64::MAX), Some(0));
    }

    #[test]
    fn a_guest_that_has_not_been_read_yet_says_so_rather_than_nothing() {
        let (mut engine, _) = Engine::try_new(config(), boot()).expect("a workable boot");

        assert_eq!(
            engine.world.release_budget(u64::MAX),
            None,
            "the agent answered for a guest it had not read",
        );

        engine.world.observe(snap(BOOT, CALM, 0));

        assert!(engine.world.release_budget(u64::MAX).is_some());
    }

    #[test]
    fn only_the_host_can_say_how_much_of_the_guest_is_hot_plugged() {
        let boot = BootInfo {
            block_size: bytes(BLOCK),
            total_mem: BOOT + 4 * BLOCK,
        };

        let (mut engine, _) = Engine::try_new(config(), boot).expect("a workable boot");
        engine.world.observe(snap(BOOT + 4 * BLOCK, 0, 0));

        assert_eq!(engine.world.release_budget(u64::MAX), Some(0));

        reconnect(&mut engine, at(0), CEILING, 4 * BLOCK);

        assert_eq!(engine.world.release_budget(u64::MAX), Some(4 * BLOCK));
    }

    #[test]
    fn a_floor_taken_before_a_plug_lands_corrects_itself_when_it_does() {
        let (mut engine, _) = Engine::try_new(config(), boot()).expect("a workable boot");
        engine.world.observe(snap(BOOT + 4 * BLOCK, 0, 0));

        connect(&mut engine, at(0), CEILING, HOUR);

        assert_eq!(engine.world.release_budget(u64::MAX), Some(0));

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
            engine.total_mem() - engine.world.release_budget(u64::MAX).expect("a reading"),
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
        engine.world.observe(snap(BOOT + 4 * BLOCK, 0, 0));

        reconnect(&mut engine, at(0), CEILING, 4 * BLOCK);

        assert_eq!(engine.world.release_budget(u64::MAX), Some(4 * BLOCK));

        host(
            &mut engine,
            at(1),
            HostMessage::Geometry {
                hotplugged: 2 * BLOCK,
            },
        );

        assert_eq!(engine.world.release_budget(u64::MAX), Some(2 * BLOCK));

        engine.handle(Event::TotalChanged {
            now: at(2),
            total_mem: BOOT + 2 * BLOCK,
        });

        assert_eq!(engine.world.release_budget(u64::MAX), Some(2 * BLOCK));
    }

    #[test]
    fn a_reconnect_takes_the_floor_the_host_gives_it_that_time() {
        let boot = BootInfo {
            block_size: bytes(BLOCK),
            total_mem: BOOT + 4 * BLOCK,
        };

        let (mut engine, _) = Engine::try_new(config(), boot).expect("a workable boot");
        engine.world.observe(snap(BOOT + 4 * BLOCK, 0, 0));

        reconnect(&mut engine, at(0), CEILING, 4 * BLOCK);
        engine.handle(Event::Disconnected { now: at(1) });

        engine.handle(Event::TotalChanged {
            now: at(2),
            total_mem: BOOT + 2 * BLOCK,
        });
        reconnect(&mut engine, at(3), CEILING, 2 * BLOCK);

        assert_eq!(engine.world.release_budget(u64::MAX), Some(2 * BLOCK));
    }

    #[derive(Debug, Clone, Copy)]
    enum Move {
        Tick {
            total_mem: u64,
            usage: u64,
            free: u64,
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

    fn step() -> impl Strategy<Value = Move> {
        prop_oneof![
            8 => (
                byte_count(),
                byte_count(),
                byte_count(),
                byte_count(),
                0_u64..8,
                0.0_f32..100.0
            )
                .prop_map(|(total_mem, usage, free, available, high_events, psi)| {
                    Move::Tick {
                        total_mem,
                        usage,
                        free,
                        available,
                        high_events,
                        psi,
                    }
                }),
            4 => (0_u64..8).prop_map(|high_events| Move::Wake { high_events }),
            3 => byte_count().prop_map(|total_mem| Move::Plug { total_mem }),
            2 => byte_count().prop_map(|reclaimed| Move::Reclaimed { reclaimed }),
            2 => byte_count().prop_map(|ceiling| Move::Connect { ceiling }),
            1 => Just(Move::Disconnect),
            2 => byte_count().prop_map(|ceiling| Move::Ceiling { ceiling }),
            4 => (any::<bool>(), byte_count(), deny_reason()).prop_map(|(grant, target, reason)| {
                Move::Answer {
                    grant,
                    target,
                    reason,
                }
            }),
            2 => (0_u64..4, byte_count()).prop_map(|(id, desired)| Move::Reclaim { id, desired }),
        ]
    }

    fn timeline() -> impl Strategy<Value = Vec<(u64, Move)>> {
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

        if matches!(engine.phase(), Phase::Steady(_)) && engine.pending_request().is_none() {
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

    fn drive(timeline: &[(u64, Move)]) -> Result<(), TestCaseError> {
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
                Move::Tick {
                    total_mem,
                    usage,
                    free,
                    available,
                    high_events,
                    psi,
                } => Event::Tick {
                    now,
                    snapshot: Snapshot {
                        total_mem,
                        workload_usage: usage,
                        free,
                        available,
                        swap_used: 0,
                        high_events,
                        max_events: 0,
                        psi_some10: psi,
                        psi_full10: psi,
                    },
                },
                Move::Wake { high_events } => Event::ThrottleWake {
                    now,
                    high_events,
                    max_events: 0,
                },
                Move::Plug { total_mem } => Event::TotalChanged { now, total_mem },
                Move::Reclaimed { reclaimed } => Event::ReclaimFinished { now, reclaimed },
                Move::Connect { ceiling } => {
                    model.connected = true;
                    model.last_id = 0;

                    Event::Connected {
                        now,
                        ceiling,
                        status_interval: Duration::from_millis(500),
                        hotplugged: 0,
                    }
                }
                Move::Disconnect => {
                    model.connected = false;
                    Event::Disconnected { now }
                }
                Move::Ceiling { ceiling } => Event::Host {
                    now,
                    message: HostMessage::Ceiling { ceiling },
                },
                Move::Answer {
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
                Move::Reclaim { id, desired } => Event::Host {
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
