use std::{
    num::{NonZeroU32, NonZeroU64},
    time::Duration,
};

use pazari_proto::{GrantReason, GuestMessage, HostMessage, Urgency};
use pazari_types::{Delay, Interval, Psi, Swappiness};

use crate::engine::{
    Action, BootInfo, Engine, EngineConfig, Event, ExhaustionMode, IdleThreshold, Mode, Snapshot,
    Timestamp,
};

pub const MIB: u64 = 1 << 20;

pub const BLOCK: u64 = 128 * MIB;
pub const BOOT: u64 = 2048 * MIB;
pub const SERVICE: u64 = 256 * MIB;
pub const BURST: u64 = 256 * MIB;
pub const GUARD: u64 = 64 * MIB;
pub const CEILING: u64 = 4096 * MIB;

pub const MAX0: u64 = BOOT - SERVICE;
pub const HIGH0: u64 = MAX0 - BURST;

pub const OPEN0: u64 = MAX0 - GUARD;

pub const BUSY: u64 = 1200 * MIB;
pub const CALM: u64 = 500 * MIB;
pub const STALLED: u64 = 1700 * MIB;

pub const HOUR: Duration = Duration::from_secs(3600);

pub const fn bytes(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("a positive byte count")
}

pub const fn count(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("a positive count")
}

pub fn interval(secs: u64) -> Interval {
    Interval::try_new(Duration::from_secs(secs)).expect("a positive interval under a day")
}

pub fn delay(secs: u64) -> Delay {
    Delay::try_new(Duration::from_secs(secs)).expect("a delay under a day")
}

pub fn config() -> EngineConfig {
    EngineConfig {
        service_reserve: SERVICE,
        burst_reserve: bytes(BURST),
        open_guard: bytes(GUARD),
        grow_step: bytes(512 * MIB),
        deny_backoff: interval(5),
        settle_grace: interval(10),
        idle_threshold: IdleThreshold::try_from(0.5).expect("a half is a deadband"),
        idle_intervals: count(3),
        shrink_step: bytes(256 * MIB),
        shrink_cooldown: delay(30),
        reclaim_swappiness: Some(Swappiness::NONE),
        titrate_step: bytes(64 * MIB),
        titrate_interval: interval(10),
        titrate_psi_threshold: Psi::try_new(5.0).expect("5% is a psi"),
        exhaustion: ExhaustionMode::Stall {
            timeout: interval(12),
        },
        tick_interval: interval(1),
    }
}

pub const fn boot() -> BootInfo {
    BootInfo {
        boot_mem: bytes(BOOT),
        block_size: bytes(BLOCK),
        plugged: BOOT,
    }
}

pub const fn snap(plugged: u64, usage: u64, high_events: u64) -> Snapshot {
    Snapshot {
        plugged,
        workload_usage: usage,
        available: plugged.saturating_sub(usage),
        swap_used: 0,
        high_events,
        max_events: 0,
        psi_some10: 0.0,
        psi_full10: 0.0,
    }
}

pub const fn snap_psi(plugged: u64, usage: u64, psi_some10: f32) -> Snapshot {
    Snapshot {
        psi_some10,
        ..snap(plugged, usage, 0)
    }
}

pub const fn at(secs: u64) -> Timestamp {
    Timestamp::from_millis(secs.saturating_mul(1_000))
}

pub fn tick(engine: &mut Engine, now: Timestamp, snapshot: Snapshot) -> Vec<Action> {
    engine.handle(Event::Tick { now, snapshot })
}

pub fn wake(engine: &mut Engine, now: Timestamp, high_events: u64) -> Vec<Action> {
    engine.handle(Event::ThrottleWake {
        now,
        high_events,
        max_events: 0,
    })
}

pub fn host(engine: &mut Engine, now: Timestamp, message: HostMessage) -> Vec<Action> {
    engine.handle(Event::Host { now, message })
}

pub const fn request(id: u64, current: u64, target: u64) -> Action {
    Action::Send(GuestMessage::GrantRequest {
        id,
        current,
        target,
        reason: GrantReason::Throttle,
        urgency: Urgency::High,
    })
}

pub fn connect(engine: &mut Engine, now: Timestamp, ceiling: u64, status: Duration) -> Vec<Action> {
    engine.handle(Event::Connected {
        now,
        ceiling,
        status_interval: status,
    })
}

pub fn titrated(config: EngineConfig, ceiling: u64) -> Engine {
    let (mut engine, _) = Engine::try_new(config, boot()).expect("a workable boot");

    connect(&mut engine, at(0), ceiling, HOUR);

    for step in 0..=4 {
        tick(&mut engine, at(step * 10), snap(BOOT, BUSY, 0));
    }

    assert_eq!(engine.mode(), Mode::Steady);
    assert_eq!(engine.high(), HIGH0);

    engine
}

pub fn started() -> Engine {
    titrated(config(), CEILING)
}

pub fn grown(engine: &mut Engine) {
    assert_eq!(
        wake(engine, at(100), 1),
        vec![Action::SetHigh(OPEN0), request(1, BOOT, 2560 * MIB)]
    );

    host(
        engine,
        at(101),
        HostMessage::Grant {
            id: 1,
            target: 2560 * MIB,
        },
    );

    assert_eq!(
        engine.handle(Event::PluggedChanged {
            now: at(102),
            plugged: 2560 * MIB,
        }),
        vec![Action::SetMax(2304 * MIB), Action::SetHigh(2048 * MIB)]
    );
}
