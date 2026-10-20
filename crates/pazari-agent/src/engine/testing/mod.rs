use std::{
    num::{NonZeroU32, NonZeroU64},
    time::Duration,
};

use pazari_proto::{DenyReason, GuestMessage, HostMessage};
pub use pazari_proto::{ReclaimId, RequestId};
use pazari_types::{Delay, Interval, Swappiness, Timestamp};

use crate::engine::{
    Action, BootInfo, Engine, EngineConfig, Event, ExhaustionMode, ExponentialConfig, GrowConfig,
    Phase, ReleaseConfig, Snapshot, SweepConfig, ThrottleMode, TitrateConfig,
};

pub const MIB: u64 = 1 << 20;

pub const BLOCK: u64 = 128 * MIB;
pub const BOOT: u64 = 2048 * MIB;
pub const SERVICE: u64 = 256 * MIB;
pub const BURST: u64 = 256 * MIB;
pub const CEILING: u64 = 4096 * MIB;

pub const MAX0: u64 = BOOT - SERVICE;
pub const HIGH0: u64 = MAX0 - BURST;

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

pub fn titrate() -> TitrateConfig {
    TitrateConfig {
        step: bytes(256 * MIB),
        intervals: count(3),
        cooldown: delay(30),
    }
}

pub fn sweep() -> SweepConfig {
    SweepConfig {
        min: bytes(256 * MIB),
        hold: delay(5),
        cooldown: delay(30),
    }
}

pub fn config() -> EngineConfig {
    EngineConfig {
        service_reserve: SERVICE,
        burst_reserve: bytes(BURST),
        throttle: ThrottleMode::Grow,
        grow: GrowConfig::Exponential(ExponentialConfig {
            step: bytes(512 * MIB),
            escalate_within: interval(10),
        }),
        deny_backoff: interval(5),
        settle_grace: interval(10),
        recovery_intervals: count(3),
        release: ReleaseConfig::Titrate(titrate()),
        reclaim_swappiness: Some(Swappiness::NONE),
        exhaustion: ExhaustionMode::Stall {
            timeout: interval(12),
        },
        tick_interval: interval(1),
    }
}

pub const fn boot() -> BootInfo {
    BootInfo {
        block_size: bytes(BLOCK),
        total_mem: BOOT,
    }
}

pub const fn snap(total_mem: u64, usage: u64, high_events: u64) -> Snapshot {
    Snapshot {
        total_mem,
        workload_usage: usage,
        free: 0,
        available: total_mem.saturating_sub(usage),
        swap_used: 0,
        high_events,
        max_events: 0,
        psi_some10: 0.0,
        psi_full10: 0.0,
    }
}

pub const fn snap_breached(total_mem: u64, usage: u64, high_events: u64) -> Snapshot {
    Snapshot {
        max_events: 1,
        ..snap(total_mem, usage, high_events)
    }
}

pub const fn snap_free(total_mem: u64, usage: u64, free: u64, high_events: u64) -> Snapshot {
    Snapshot {
        free,
        ..snap(total_mem, usage, high_events)
    }
}

pub const fn snap_available(
    total_mem: u64,
    usage: u64,
    available: u64,
    high_events: u64,
) -> Snapshot {
    Snapshot {
        available,
        ..snap(total_mem, usage, high_events)
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

pub fn reclaimed(engine: &mut Engine, now: Timestamp, reclaimed: u64) -> Vec<Action> {
    engine.handle(Event::ReclaimFinished { now, reclaimed })
}

pub fn host(engine: &mut Engine, now: Timestamp, message: HostMessage) -> Vec<Action> {
    engine.handle(Event::Host { now, message })
}

pub const fn request(id: u64, current: u64, target: u64) -> Action {
    Action::Send(GuestMessage::GrantRequest {
        id: RequestId(id),
        current,
        target,
    })
}

pub const fn reclaim(bytes: u64) -> Action {
    Action::StartReclaim {
        bytes,
        swappiness: Some(Swappiness::NONE),
    }
}

pub const fn deny(id: RequestId) -> HostMessage {
    HostMessage::Deny {
        id,
        reason: DenyReason::Ceiling,
        retry_after: None,
    }
}

pub fn connect(engine: &mut Engine, now: Timestamp, ceiling: u64, status: Duration) -> Vec<Action> {
    engine.handle(Event::Connected {
        now,
        ceiling,
        status_interval: status,
        hotplugged: 0,
    })
}

pub fn reconnect(
    engine: &mut Engine,
    now: Timestamp,
    ceiling: u64,
    hotplugged: u64,
) -> Vec<Action> {
    engine.handle(Event::Connected {
        now,
        ceiling,
        status_interval: HOUR,
        hotplugged,
    })
}

pub fn started_with(config: EngineConfig, ceiling: u64) -> Engine {
    let (mut engine, _) = Engine::try_new(config, boot()).expect("a workable boot");

    connect(&mut engine, at(0), ceiling, HOUR);
    tick(&mut engine, at(0), snap(BOOT, BUSY, 0));

    assert!(matches!(engine.phase(), Phase::Steady(_)));
    assert_eq!(engine.high(), HIGH0);

    engine
}

pub fn started() -> Engine {
    started_with(config(), CEILING)
}

pub fn grown(engine: &mut Engine) {
    assert_eq!(wake(engine, at(100), 1), vec![request(1, BOOT, 2560 * MIB)]);

    host(
        engine,
        at(101),
        HostMessage::Grant {
            id: RequestId(1),
            target: 2560 * MIB,
        },
    );

    assert_eq!(
        engine.handle(Event::TotalChanged {
            now: at(102),
            total_mem: 2560 * MIB,
        }),
        vec![Action::SetMax(2304 * MIB), Action::SetHigh(2048 * MIB)]
    );

    assert_eq!(
        host(
            engine,
            at(103),
            HostMessage::Geometry {
                hotplugged: 2560 * MIB - BOOT,
            },
        ),
        vec![]
    );
}

/// Grows the engine and holds it calm until its release's reclaim is running.
pub fn releasing(engine: &mut Engine) {
    tick(engine, at(99), snap(BOOT, STALLED, 0));
    grown(engine);

    for second in 140..=143 {
        tick(engine, at(second), snap(2560 * MIB, CALM, 1));
    }

    assert!(matches!(engine.phase(), Phase::Releasing(_)));
}
