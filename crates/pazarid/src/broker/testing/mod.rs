use std::{
    num::{NonZeroU32, NonZeroU64},
    path::PathBuf,
    sync::LazyLock,
    time::Duration,
};

use pazari_proto::{
    DenyReason, GrantReason, GuestMessage, HostMessage, PROTOCOL_VERSION, Status, Urgency,
};
use pazari_transport::PeerId;
use pazari_types::{Interval, Psi, Timestamp};

use crate::broker::{
    Action, Ask, Broker, Event, GuestId, GuestSpec, GuestView, HostView, Policy, Registry, Request,
    Revocation,
};

pub const MIB: u64 = 1 << 20;
pub const GIB: u64 = 1 << 30;

pub const BLOCK: u64 = 128 * MIB;
pub const BOOT: u64 = 2048 * MIB;
pub const CEILING: u64 = 4096 * MIB;

pub const FLOOR: u64 = GIB;
pub const HOST: u64 = 8 * GIB;

pub const ONE: u32 = 3;
pub const TWO: u32 = 4;

pub const REVOKE_PSI: f64 = 20.0;
pub const REVOKE_STEP: u64 = 256 * MIB;

pub static POLICY: LazyLock<Policy> = LazyLock::new(policy);

pub static GRANTING: LazyLock<Policy> = LazyLock::new(granting);

pub fn interval(secs: u64) -> Interval {
    Interval::try_new(Duration::from_secs(secs)).expect("a positive interval under a day")
}

pub const fn count(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("a positive count")
}

pub const fn bytes(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("a positive byte count")
}

pub const fn at(secs: u64) -> Timestamp {
    Timestamp::from_millis(secs.saturating_mul(1_000))
}

pub const fn vsock(cid: u32) -> PeerId {
    PeerId::Vsock { cid }
}

pub fn unix(path: &str) -> PeerId {
    PeerId::Unix {
        path: PathBuf::from(path),
    }
}

pub fn policy() -> Policy {
    Policy {
        welcome_status_interval: interval(1),
        host_floor: FLOOR,
        grant_min_interval: interval(5),
        reconcile_deadline: interval(30),
        shrink_revert: true,
        stale_after: count(3),
        reclaim_deadline: interval(30),
        grant_swap: false,
        revocation: None,
    }
}

pub fn granting() -> Policy {
    Policy {
        grant_swap: true,
        ..policy()
    }
}

pub fn revoking() -> Policy {
    Policy {
        revocation: Some(Revocation {
            psi: Psi::try_new(REVOKE_PSI).expect("a fifth of the wall clock is a psi"),
            step: bytes(REVOKE_STEP),
        }),
        ..policy()
    }
}

pub fn guest(name: &str, ceiling: u64) -> GuestSpec {
    GuestSpec {
        name: name.to_owned(),
        ceiling: bytes(ceiling),
    }
}

pub fn registry() -> Registry {
    let mut registry = Registry::new();

    registry
        .insert(vsock(ONE), guest("one", CEILING))
        .expect("the first guest");
    registry
        .insert(vsock(TWO), guest("two", CEILING))
        .expect("the second guest");

    registry
}

pub fn id(cid: u32) -> GuestId {
    registry().lookup(&vsock(cid)).expect("a registered guest")
}

pub const fn available(now: Timestamp, bytes: u64) -> Event {
    Event::HostAvailable {
        now,
        bytes,
        swap_free: 0,
        psi: None,
    }
}

pub const fn pressured(now: Timestamp, bytes: u64, swap_free: u64, psi: f32) -> Event {
    Event::HostAvailable {
        now,
        bytes,
        swap_free,
        psi: Some(psi),
    }
}

pub fn sized() -> Broker {
    sized_with(policy())
}

pub fn sized_with(policy: Policy) -> Broker {
    let mut broker = Broker::new(policy, registry());

    let _ = broker.handle(available(at(0), HOST));

    for cid in [ONE, TWO] {
        let _ = broker.handle(Event::Size {
            now: at(0),
            guest: id(cid),
            size: BOOT,
        });
    }

    broker
}

pub fn greet(broker: &mut Broker, now: Timestamp, cid: u32) {
    connect(broker, now, cid);

    assert_eq!(
        message(broker, now, cid, hello()),
        vec![welcome(cid, CEILING)],
    );
}

pub fn welcomed(cid: u32) -> Broker {
    let mut broker = sized();

    greet(&mut broker, at(0), cid);

    broker
}

pub fn connect(broker: &mut Broker, now: Timestamp, cid: u32) -> Vec<Action> {
    broker.handle(Event::Connected {
        now,
        peer: vsock(cid),
    })
}

pub fn message(
    broker: &mut Broker,
    now: Timestamp,
    cid: u32,
    message: GuestMessage,
) -> Vec<Action> {
    broker.handle(Event::Message {
        now,
        peer: vsock(cid),
        message,
    })
}

pub fn tick(broker: &mut Broker, now: Timestamp) -> Vec<Action> {
    broker.handle(Event::Tick { now })
}

pub fn sizes(broker: &mut Broker, now: Timestamp, cid: u32, size: u64) -> Vec<Action> {
    broker.handle(Event::Size {
        now,
        guest: id(cid),
        size,
    })
}

pub fn hello() -> GuestMessage {
    GuestMessage::Hello {
        protocol: PROTOCOL_VERSION,
        agent_version: "0.1.0".to_owned(),
        boot_mem: BOOT,
        block_size: BLOCK,
        plugged: BOOT,
    }
}

pub const fn ask(id: u64, current: u64, target: u64) -> GuestMessage {
    GuestMessage::GrantRequest {
        id,
        current,
        target,
        reason: GrantReason::Throttle,
        urgency: Urgency::High,
    }
}

pub const fn release(id: u64, current: u64, releasable: u64) -> GuestMessage {
    GuestMessage::OfferRelease {
        id,
        current,
        releasable,
    }
}

pub const fn report(plugged: u64, available: u64) -> GuestMessage {
    GuestMessage::Status(status(plugged, available))
}

pub const fn status(plugged: u64, available: u64) -> Status {
    Status {
        plugged,
        available,
        high_events: 0,
        max_events: 0,
        psi_some10: 0.0,
        psi_full10: 0.0,
        swap_used: 0,
        workload_usage: plugged.saturating_sub(available),
    }
}

pub fn welcome(cid: u32, ceiling: u64) -> Action {
    Action::Send {
        peer: vsock(cid),
        message: HostMessage::Welcome {
            protocol: PROTOCOL_VERSION,
            ceiling,
            status_interval_ms: policy().welcome_status_interval.get(),
        },
    }
}

pub const fn grant(cid: u32, id: u64, target: u64) -> Action {
    Action::Send {
        peer: vsock(cid),
        message: HostMessage::Grant { id, target },
    }
}

pub const fn deny(cid: u32, id: u64, reason: DenyReason) -> Action {
    Action::Send {
        peer: vsock(cid),
        message: HostMessage::Deny {
            id,
            reason,
            retry_after_ms: None,
        },
    }
}

pub const fn retry(cid: u32, id: u64, reason: DenyReason, after: Duration) -> Action {
    Action::Send {
        peer: vsock(cid),
        message: HostMessage::Deny {
            id,
            reason,
            retry_after_ms: Some(after),
        },
    }
}

pub fn requested(cid: u32, bytes: u64) -> Action {
    Action::SetRequested {
        guest: id(cid),
        bytes,
    }
}

pub const fn closed(cid: u32) -> Action {
    Action::Close { peer: vsock(cid) }
}

pub fn view(spec: &GuestSpec, size: u64, requested: Option<u64>) -> GuestView<'_> {
    GuestView {
        id: GuestId::from_index(0),
        spec,
        boot_mem: Some(BOOT),
        size,
        requested,
        status: None,
        stale: false,
        connected: true,
        granted: None,
    }
}

pub const fn offer(target: u64) -> Ask {
    Ask::Release { target }
}

pub fn request<'a>(
    now: Timestamp,
    guest: GuestView<'a>,
    ask: Ask,
    others: &'a [GuestView<'a>],
    available: Option<u64>,
) -> Request<'a> {
    Request {
        now,
        guest,
        ask,
        others,
        host: HostView {
            available,
            swap_free: None,
            psi: None,
        },
        policy: &POLICY,
    }
}

pub fn credited<'a>(
    now: Timestamp,
    guest: GuestView<'a>,
    ask: Ask,
    others: &'a [GuestView<'a>],
    available: Option<u64>,
    swap_free: Option<u64>,
) -> Request<'a> {
    Request {
        now,
        guest,
        ask,
        others,
        host: HostView {
            available,
            swap_free,
            psi: None,
        },
        policy: &GRANTING,
    }
}
