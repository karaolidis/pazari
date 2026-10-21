use std::{
    num::{NonZeroU32, NonZeroU64},
    path::PathBuf,
    sync::LazyLock,
    time::Duration,
};

use pazari_proto::{DenyReason, GuestMessage, HostMessage, PROTOCOL_VERSION, Status};
pub use pazari_proto::{ReclaimId, RequestId};
use pazari_transport::PeerId;
use pazari_types::{Interval, Psi, Timestamp};

use crate::broker::{
    Action, Ask, Broker, Effects, Event, Geometry, Grantable, GuestId, GuestSpec, GuestView,
    HostView, Policy, Registry, Request, Revocation, StalledShrinks,
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

/// What the guest's kernel keeps for itself out of the device's size.
pub const RESERVED: u64 = 136 * MIB;

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
        status_interval: interval(1),
        stale_after: count(3),
        grant_floor: FLOOR,
        grant_min_interval: interval(5),
        grantable: Grantable::Ram,
        actuation_deadline: interval(30),
        stalled_shrinks: StalledShrinks::Revert,
        reclaim_deadline: interval(30),
        revocation: None,
    }
}

pub fn granting() -> Policy {
    Policy {
        grantable: Grantable::RamAndSwap,
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

pub fn registry() -> Registry<()> {
    let mut registry = Registry::new();

    registry
        .insert(vsock(ONE), guest("one", CEILING), ())
        .expect("the first guest");
    registry
        .insert(vsock(TWO), guest("two", CEILING), ())
        .expect("the second guest");

    registry
}

pub fn id(cid: u32) -> GuestId {
    registry().lookup(&vsock(cid)).expect("a registered guest")
}

pub const fn available(bytes: u64) -> Event {
    Event::HostAvailable {
        bytes,
        swap_free: 0,
        psi: None,
    }
}

pub const fn pressured(bytes: u64, swap_free: u64, psi: f64) -> Event {
    Event::HostAvailable {
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

    let _ = broker.handle(available(HOST));

    for cid in [ONE, TWO] {
        let _ = broker.handle(Event::Size {
            now: at(0),
            guest: id(cid),
            size: BOOT,
            hotplugged: 0,
        });
    }

    broker
}

pub fn greet(broker: &mut Broker, now: Timestamp, cid: u32) {
    greet_at(broker, now, cid, BOOT, 0);
}

pub fn greet_at(broker: &mut Broker, now: Timestamp, cid: u32, total_mem: u64, hotplugged: u64) {
    connect(broker, cid);

    assert_eq!(
        message(broker, now, cid, hello_at(total_mem)),
        vec![welcome(cid, CEILING, hotplugged)],
    );
}

pub fn welcomed(cid: u32) -> Broker {
    let mut broker = sized();

    greet(&mut broker, at(0), cid);

    broker
}

pub fn plugged(cid: u32) -> Broker {
    let mut broker = welcomed(cid);

    sizes(&mut broker, at(1), cid, BOOT + GIB);

    broker
}

/// A lone guest welcomed at its boot memory under `ceiling`.
pub fn ceilinged(ceiling: u64) -> Broker {
    let mut registry = Registry::new();

    registry
        .insert(vsock(ONE), guest("one", ceiling), ())
        .expect("the only guest");

    let mut broker = Broker::new(policy(), registry);

    let _ = broker.handle(available(HOST));
    sizes(&mut broker, at(0), ONE, BOOT);
    connect(&mut broker, ONE);

    assert_eq!(
        message(&mut broker, at(0), ONE, hello()),
        vec![welcome(ONE, ceiling, 0)],
    );

    broker
}

/// A guest whose kernel keeps [`RESERVED`] of its device for itself.
pub fn reserving() -> Broker {
    let mut broker = sized();

    greet_at(&mut broker, at(0), ONE, BOOT - RESERVED, 0);

    broker
}

/// A guest whose hello waits on its device, which has not reported.
pub fn held(total_mem: u64) -> Broker {
    let mut broker = Broker::new(policy(), registry());

    let _ = broker.handle(available(HOST));

    connect(&mut broker, ONE);

    assert_eq!(
        message(&mut broker, at(0), ONE, hello_at(total_mem)),
        vec![],
        "the host answered before it knew how much of the guest was plugged in",
    );

    broker
}

/// Both guests welcomed, under revocation, at the sizes given.
pub fn holding(one: u64, two: u64) -> Broker {
    let mut broker = sized_with(revoking());

    greet(&mut broker, at(0), ONE);
    greet(&mut broker, at(0), TWO);

    sizes(&mut broker, at(1), ONE, one);
    sizes(&mut broker, at(1), TWO, two);

    broker
}

pub fn connect(broker: &mut Broker, cid: u32) -> Vec<Action> {
    broker.handle(Event::Connected { peer: vsock(cid) })
}

pub fn disconnect(broker: &mut Broker, cid: u32) -> Vec<Action> {
    broker.handle(Event::Disconnected { peer: vsock(cid) })
}

pub fn reclaim(broker: &mut Broker, now: Timestamp, cid: u32, desired: u64) -> Vec<Action> {
    let mut fx = Effects::default();

    broker.on_reclaim(now, id(cid), desired, &mut fx);

    fx.drain()
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
        hotplugged: size.saturating_sub(BOOT),
    })
}

pub fn hello() -> GuestMessage {
    hello_at(BOOT)
}

pub fn hello_at(total_mem: u64) -> GuestMessage {
    GuestMessage::Hello {
        protocol: PROTOCOL_VERSION,
        agent_version: "0.1.0".to_owned(),
        block_size: bytes(BLOCK),
        total_mem,
    }
}

pub const fn ask(id: u64, current: u64, target: u64) -> GuestMessage {
    GuestMessage::GrantRequest {
        id: RequestId(id),
        current,
        target,
    }
}

pub const fn release(id: u64, current: u64, releasable: u64) -> GuestMessage {
    GuestMessage::OfferRelease {
        id: RequestId(id),
        current,
        releasable,
    }
}

pub const fn ack(id: u64, will_release: u64) -> GuestMessage {
    GuestMessage::ReclaimAck {
        id: ReclaimId(id),
        will_release,
    }
}

pub fn nak(id: u64) -> GuestMessage {
    GuestMessage::ReclaimNak {
        id: ReclaimId(id),
        reason: "the workload is pinned".to_owned(),
    }
}

pub const fn report(total_mem: u64, available: u64) -> GuestMessage {
    GuestMessage::Status(status(total_mem, available))
}

pub const fn status(total_mem: u64, available: u64) -> Status {
    Status {
        total_mem,
        available,
        high_events: 0,
        max_events: 0,
        psi_some10: 0.0,
        psi_full10: 0.0,
        swap_used: 0,
        workload_usage: total_mem.saturating_sub(available),
    }
}

pub fn welcome(cid: u32, ceiling: u64, hotplugged: u64) -> Action {
    Action::Send {
        peer: vsock(cid),
        message: HostMessage::Welcome {
            protocol: PROTOCOL_VERSION,
            ceiling,
            status_interval: policy().status_interval.get(),
            hotplugged,
        },
    }
}

pub fn geometry(cid: u32, hotplugged: u64) -> Action {
    Action::Send {
        peer: vsock(cid),
        message: HostMessage::Geometry { hotplugged },
    }
}

pub const fn grant(cid: u32, id: u64, target: u64) -> Action {
    Action::Send {
        peer: vsock(cid),
        message: HostMessage::Grant {
            id: RequestId(id),
            target,
        },
    }
}

pub const fn deny(cid: u32, id: u64, reason: DenyReason) -> Action {
    Action::Send {
        peer: vsock(cid),
        message: HostMessage::Deny {
            id: RequestId(id),
            reason,
            retry_after: None,
        },
    }
}

pub const fn retry(cid: u32, id: u64, reason: DenyReason, after: Duration) -> Action {
    Action::Send {
        peer: vsock(cid),
        message: HostMessage::Deny {
            id: RequestId(id),
            reason,
            retry_after: Some(after),
        },
    }
}

pub fn reclaimed(cid: u32, id: u64, desired: u64) -> Action {
    Action::Send {
        peer: vsock(cid),
        message: HostMessage::ReclaimRequest {
            id: ReclaimId(id),
            desired,
            deadline: policy().reclaim_deadline.get(),
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
        geometry: Geometry {
            size,
            hotplugged: 0,
            offset: None,
        },
        requested,
        session: None,
        granted: None,
    }
}

pub const fn offer(target: u64) -> Ask {
    Ask::Release { target }
}

pub const fn host(available: Option<u64>, swap_free: Option<u64>) -> HostView {
    HostView {
        available,
        swap_free,
        psi: None,
    }
}

pub const fn request<'a>(
    now: Timestamp,
    guest: GuestView<'a>,
    ask: Ask,
    others: &'a [GuestView<'a>],
    host: HostView,
    policy: &'a Policy,
) -> Request<'a> {
    Request {
        now,
        guest,
        ask,
        others,
        host,
        policy,
    }
}
