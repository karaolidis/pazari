//! The agent and the daemon negotiating over a real socket.

use std::{
    collections::VecDeque,
    future::{self, Future},
    num::{NonZeroU32, NonZeroU64},
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use pazari_agent::{
    engine::{
        Action as GuestAction, BootInfo, Engine, EngineConfig, Event as GuestEvent, ExhaustionMode,
        ExponentialConfig, GrowConfig, Phase as GuestPhase, ReleaseConfig, Snapshot, ThrottleMode,
        TitrateConfig,
    },
    session::{Geometry, Outbound, Session, SessionConfig},
};
use pazari_proto::{
    DenyReason, GuestMessage, HostMessage, PROTOCOL_VERSION, ReclaimId, RequestId, Status,
};
use pazari_transport::{
    AnyConnection, AnyConnector, AnyListener, Listener, PeerId, UnixConnector, UnixListener,
};
use pazari_types::{Clock, Delay, Interval, Psi, Timestamp};
use pazarid::{
    broker::{
        Action, Broker, Event, Guest, GuestId, GuestSpec, Negotiated, Policy, Registry, Revocation,
        Session as BrokerSession,
    },
    run::{Peers, Report},
    runtime::{Actuator, Driver, Reading, RuntimeError},
};
use tempfile::TempDir;
use tokio::{
    sync::{mpsc, watch},
    time::timeout,
};

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

const BLOCK: u64 = 128 * MIB;
const BOOT: u64 = 2048 * MIB;
const SERVICE: u64 = 256 * MIB;
const BURST: u64 = 256 * MIB;
const CEILING: u64 = 4096 * MIB;

const FLOOR: u64 = GIB;
const PLENTY: u64 = 8 * GIB;

const BUSY: u64 = 1200 * MIB;
const CALM: u64 = 500 * MIB;

const GROWN: u64 = 2560 * MIB;
const SHRUNK: u64 = 2304 * MIB;
const REGROWN: u64 = 3072 * MIB;

const RELEASE: u64 = 256 * MIB;

const REVOKE_PSI: f64 = 20.0;
const PRESSED: f64 = 25.0;

const STEP: Duration = Duration::from_secs(1);

const PATIENCE: Duration = Duration::from_secs(10);

const LULL: Duration = Duration::from_millis(100);

const DEPTH: usize = 64;

const fn bytes(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("a positive byte count")
}

const fn count(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("a positive count")
}

fn brief() -> Interval {
    Interval::try_new(Duration::from_millis(1)).expect("a millisecond")
}

fn patient() -> Interval {
    Interval::try_new(PATIENCE).expect("a positive interval under a day")
}

fn seconds(value: u64) -> Interval {
    Interval::try_new(Duration::from_secs(value)).expect("a positive interval under a day")
}

fn policy() -> Policy {
    Policy {
        welcome_status_interval: seconds(1),
        host_floor: FLOOR,
        grant_min_interval: brief(),
        reconcile_deadline: seconds(30),
        shrink_revert: true,
        stale_after: count(3),
        reclaim_deadline: seconds(30),
        grant_swap: false,
        revocation: None,
    }
}

fn revoking() -> Policy {
    Policy {
        revocation: Some(Revocation {
            psi: Psi::try_new(REVOKE_PSI).expect("a fifth of the wall clock is a psi"),
            step: bytes(RELEASE),
        }),
        ..policy()
    }
}

fn engine_config() -> EngineConfig {
    EngineConfig {
        service_reserve: SERVICE,
        burst_reserve: bytes(BURST),
        throttle: ThrottleMode::Grow,
        grow: GrowConfig::Exponential(ExponentialConfig {
            step: bytes(512 * MIB),
            escalate_within: patient(),
            cap: count(64),
        }),
        release: ReleaseConfig::Titrate(TitrateConfig {
            step: bytes(RELEASE),
            after: count(3),
            cooldown: Delay::ZERO,
        }),
        settle_grace: patient(),
        deny_backoff: brief(),
        recover_after: count(3),
        exhaustion: ExhaustionMode::Kill,
    }
}

const fn request(id: u64, current: u64, target: u64) -> GuestMessage {
    GuestMessage::GrantRequest {
        id: RequestId(id),
        current,
        target,
    }
}

#[derive(Debug)]
struct Sizes {
    size: u64,
    requested: u64,
}

#[derive(Debug)]
struct Device {
    sizes: Mutex<Sizes>,
    completed: mpsc::UnboundedSender<u64>,
    plugs: mpsc::UnboundedSender<u64>,
    pending: tokio::sync::Mutex<mpsc::UnboundedReceiver<u64>>,
}

impl Device {
    fn new(size: u64) -> (Self, mpsc::UnboundedReceiver<u64>) {
        let (completed, pending) = mpsc::unbounded_channel();
        let (plugs, total_mem) = mpsc::unbounded_channel();

        let device = Self {
            sizes: Mutex::new(Sizes {
                size,
                requested: size,
            }),
            completed,
            plugs,
            pending: tokio::sync::Mutex::new(pending),
        };

        (device, total_mem)
    }

    fn locked(&self) -> MutexGuard<'_, Sizes> {
        self.sizes.lock().expect("no test poisons the device")
    }

    fn write(&self, bytes: u64) {
        let mut sizes = self.locked();

        sizes.requested = bytes;

        if sizes.size == bytes {
            return;
        }

        sizes.size = bytes;
        drop(sizes);

        self.completed
            .send(bytes)
            .expect("the device holds its own size channel");
        self.plugs
            .send(bytes)
            .expect("the harness holds the plug channel");
    }
}

impl Actuator for Device {
    fn reading(&self) -> impl Future<Output = Result<Reading, RuntimeError>> + Send {
        future::ready(Ok(Reading::sized(self.locked().size, BOOT)))
    }

    fn requested(&self) -> impl Future<Output = Result<u64, RuntimeError>> + Send {
        future::ready(Ok(self.locked().requested))
    }

    fn set_requested(&self, bytes: u64) -> impl Future<Output = Result<(), RuntimeError>> + Send {
        self.write(bytes);

        future::ready(Ok(()))
    }

    async fn changed(&self) -> Result<Reading, RuntimeError> {
        let mut pending = self.pending.lock().await;

        match pending.recv().await {
            Some(size) => Ok(Reading::sized(size, BOOT)),
            None => future::pending().await,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Mark {
    taken: usize,
    emitted: usize,
    heard: usize,
}

#[derive(Debug)]
enum Step {
    Accepted((AnyConnection, PeerId)),
    Reported(Report),
    Sized(Event),
    Session(GuestEvent),
    Plugged(u64),
}

struct Harness {
    _dir: TempDir,
    now: Timestamp,

    listener: AnyListener,
    peer: PeerId,
    guest: GuestId,
    accepted: u32,

    broker: Broker,
    peers: Peers,
    reports: mpsc::Receiver<Report>,
    driver: Arc<Driver<Device>>,
    sizes: mpsc::Receiver<Event>,

    engine: Engine,
    session: Session,
    outbound: Outbound,
    geometry: watch::Sender<u64>,
    plugs: mpsc::UnboundedReceiver<u64>,

    plugging: VecDeque<u64>,
    total_mem: u64,
    high_events: u64,

    taken: Vec<Action>,
    emitted: Vec<GuestAction>,
    heard: Vec<HostMessage>,
}

impl Harness {
    async fn start() -> Self {
        Self::start_with(policy()).await
    }

    async fn start_with(policy: Policy) -> Self {
        let mut harness = Self::unseeded(policy);

        harness.seed().await;

        harness
    }

    fn unseeded(policy: Policy) -> Self {
        let dir = TempDir::new().expect("a temporary directory");
        let path = dir.path().join("host.sock");

        let listener = AnyListener::Unix(UnixListener::bind(&path).expect("a bound listener"));
        let peer = listener.local();

        let mut registry = Registry::new();
        let guest = registry
            .insert(
                peer.clone(),
                GuestSpec {
                    name: "one".to_owned(),
                    ceiling: bytes(CEILING),
                },
            )
            .expect("the only guest");

        let clock = Clock::start();

        let (device, plugs) = Device::new(BOOT);
        let driver = Arc::new(Driver::new(guest, device, clock));

        let (sender, sizes) = mpsc::channel(DEPTH);
        drop(tokio::spawn(watch(Arc::clone(&driver), sender)));

        let (peers, reports) = Peers::new();

        let boot = BootInfo {
            block_size: bytes(BLOCK),
            total_mem: BOOT,
        };

        let (engine, emitted) = Engine::try_new(engine_config(), boot).expect("a workable boot");

        let geometry = watch::Sender::new(BOOT);

        let (session, outbound) = Session::spawn(
            SessionConfig {
                connector: AnyConnector::Unix(UnixConnector::new(&path)),
                handshake_timeout: seconds(5),
                reconnect_min: brief(),
                reconnect_max: Interval::try_new(Duration::from_millis(5))
                    .expect("five milliseconds"),
            },
            Geometry {
                block_size: bytes(BLOCK),
                total_mem: geometry.subscribe(),
            },
            clock,
        );

        let mut harness = Self {
            _dir: dir,
            now: Timestamp::START,
            listener,
            peer,
            guest,
            accepted: 0,
            broker: Broker::new(policy, registry),
            peers,
            reports,
            driver,
            sizes,
            engine,
            session,
            outbound,
            geometry,
            plugs,
            plugging: VecDeque::new(),
            total_mem: BOOT,
            high_events: 0,
            taken: Vec::new(),
            emitted,
            heard: Vec::new(),
        };

        harness.tick(0);

        harness
    }

    async fn seed(&mut self) {
        let seed = self
            .driver
            .seed()
            .await
            .expect("the device reports its size");

        self.host(seed).await;
    }

    fn now(&mut self) -> Timestamp {
        self.now = self.now.saturating_add(STEP);
        self.now
    }

    const fn mark(&self) -> Mark {
        Mark {
            taken: self.taken.len(),
            emitted: self.emitted.len(),
            heard: self.heard.len(),
        }
    }

    async fn host(&mut self, event: Event) {
        let actions = self.broker.handle(event);

        for action in &actions {
            match action {
                Action::Send { peer, message } => self.peers.send(peer, message.clone()),
                Action::Close { peer } => self.peers.close(peer),
                Action::SetRequested { .. } => {}
            }
        }

        self.driver
            .apply(&actions)
            .await
            .expect("the device takes every size");

        self.taken.extend(actions);
    }

    fn guest(&mut self, event: GuestEvent) {
        let mut queue = VecDeque::from([event]);

        while let Some(event) = queue.pop_front() {
            if let GuestEvent::Host { ref message, .. } = event {
                self.heard.push(message.clone());
            }

            let actions = self.engine.handle(event);

            self.geometry.send_replace(self.engine.total_mem());

            for action in &actions {
                match action {
                    GuestAction::Send(message) => self.outbound.send(message.clone()),
                    GuestAction::StartReclaim { bytes, .. } => {
                        let now = self.now;

                        queue.push_back(GuestEvent::ReclaimFinished {
                            now,
                            reclaimed: *bytes,
                        });
                    }
                    _ => {}
                }
            }

            self.emitted.extend(actions);
        }
    }

    async fn step(&mut self) {
        let step = tokio::select! {
            accepted = self.listener.accept() => Step::Accepted(accepted.expect("a connection")),
            reported = self.reports.recv() => Step::Reported(reported.expect("the harness holds a sender")),
            sized = self.sizes.recv() => Step::Sized(sized.expect("the watcher is running")),
            event = self.session.event() => Step::Session(event.expect("the session is running")),
            total_mem = self.plugs.recv() => Step::Plugged(total_mem.expect("the harness holds the device")),
        };

        match step {
            Step::Accepted((connection, peer)) => {
                self.accepted = self.accepted.saturating_add(1);
                self.peers.accept(connection, peer.clone());

                let now = self.now();
                self.host(Event::Connected { now, peer }).await;
            }
            Step::Reported(report) => {
                let now = self.now();

                if let Some(event) = self.peers.digest(report, now) {
                    self.host(event).await;
                }
            }
            Step::Sized(event) => self.host(event).await,
            Step::Session(event) => self.guest(event),
            Step::Plugged(total_mem) => self.plugging.push_back(total_mem),
        }
    }

    async fn settle(&mut self, what: &str, ready: impl Fn(&Self) -> bool) {
        let pumping = async {
            while !ready(self) {
                self.step().await;
            }
        };

        assert!(
            timeout(PATIENCE, pumping).await.is_ok(),
            "timed out waiting for {what}"
        );
    }

    async fn lull(&mut self) {
        let pumping = async {
            loop {
                self.step().await;
            }
        };

        let _ = timeout(LULL, pumping).await;
    }

    async fn handshake(&mut self) {
        self.settle("the handshake", |harness| {
            harness.welcomed() && harness.engine.is_connected() && harness.status().is_some()
        })
        .await;
    }

    async fn available(&mut self, bytes: u64) {
        let now = self.now();

        self.host(Event::HostAvailable {
            now,
            bytes,
            swap_free: 0,
            psi: None,
        })
        .await;
    }

    async fn pressured(&mut self, bytes: u64, psi: f64) {
        let now = self.now();

        self.host(Event::HostAvailable {
            now,
            bytes,
            swap_free: 0,
            psi: Some(psi),
        })
        .await;
    }

    async fn beat(&mut self) {
        let now = self.now();

        self.host(Event::Tick { now }).await;
    }

    fn tick(&mut self, workload_usage: u64) {
        let now = self.now();
        let total_mem = self.total_mem;
        let high_events = self.high_events;

        self.guest(GuestEvent::Tick {
            now,
            snapshot: Snapshot {
                total_mem,
                workload_usage,
                free: 0,
                available: total_mem.saturating_sub(workload_usage),
                swap_used: 0,
                high_events,
                max_events: 0,
                psi_some10: 0.0,
                psi_full10: 0.0,
            },
        });
    }

    fn wake(&mut self) {
        self.high_events = self.high_events.saturating_add(1);

        let now = self.now();
        let high_events = self.high_events;

        self.guest(GuestEvent::ThrottleWake {
            now,
            high_events,
            max_events: 0,
        });
    }

    fn plug(&mut self) {
        let total_mem = self
            .plugging
            .pop_front()
            .expect("the device took on a size");

        self.total_mem = total_mem;

        let now = self.now();
        self.guest(GuestEvent::TotalChanged { now, total_mem });
    }

    async fn resized(&mut self, mark: Mark, to: u64) {
        self.settle("the grant and the resize", |harness| {
            harness.granted(mark)
                && harness.size() == Some(to)
                && harness.requested().is_none()
                && !harness.plugging.is_empty()
        })
        .await;

        self.plug();
    }

    async fn grow(&mut self, to: u64) {
        let mark = self.mark();

        self.tick(BUSY);
        self.wake();

        self.resized(mark, to).await;

        self.settle("the geometry", |harness| {
            harness.heard[mark.heard..].iter().any(|message| {
                matches!(
                    message,
                    HostMessage::Geometry { hotplugged } if *hotplugged == to.saturating_sub(BOOT)
                )
            })
        })
        .await;

        assert!(
            matches!(self.engine.phase(), GuestPhase::Steady(_)),
            "the guest did not settle on the memory it was given"
        );
    }

    async fn cut(&mut self) {
        let peer = self.peer.clone();

        self.peers.close(&peer);

        let now = self.now();
        self.host(Event::Disconnected { now, peer }).await;
    }

    fn welcomed(&self) -> bool {
        self.broker.is_negotiated(&self.peer)
    }

    fn negotiated(&self) -> Option<&Negotiated> {
        self.broker
            .guest(self.guest)
            .and_then(|guest| guest.session.as_ref())
            .and_then(BrokerSession::negotiated)
    }

    fn status(&self) -> Option<Status> {
        self.negotiated().and_then(|session| session.status)
    }

    fn size(&self) -> Option<u64> {
        self.broker
            .guest(self.guest)
            .and_then(pazarid::broker::Guest::size)
    }

    fn requested(&self) -> Option<u64> {
        self.broker.guest(self.guest).and_then(Guest::requested)
    }

    fn granted(&self, mark: Mark) -> bool {
        self.heard[mark.heard..]
            .iter()
            .any(|message| matches!(message, HostMessage::Grant { .. }))
    }

    fn denied(&self, mark: Mark) -> bool {
        self.heard[mark.heard..]
            .iter()
            .any(|message| matches!(message, HostMessage::Deny { .. }))
    }

    fn sent(&self, mark: Mark) -> Vec<HostMessage> {
        self.taken[mark.taken..]
            .iter()
            .filter_map(|action| match action {
                Action::Send { message, .. } => Some(message.clone()),
                _ => None,
            })
            .collect()
    }

    fn answers(&self, mark: Mark, id: u64) -> Vec<HostMessage> {
        self.sent(mark)
            .into_iter()
            .filter(|message| {
                matches!(
                    message,
                    HostMessage::Grant { id: RequestId(answered), .. }
                    | HostMessage::Deny { id: RequestId(answered), .. } if *answered == id
                )
            })
            .collect()
    }

    fn actuated(&self, mark: Mark) -> Vec<u64> {
        self.taken[mark.taken..]
            .iter()
            .filter_map(|action| match *action {
                Action::SetRequested { bytes, .. } => Some(bytes),
                _ => None,
            })
            .collect()
    }

    fn asked(&self, mark: Mark) -> Vec<GuestMessage> {
        self.emitted[mark.emitted..]
            .iter()
            .filter_map(|action| match action {
                GuestAction::Send(
                    message @ (GuestMessage::GrantRequest { .. }
                    | GuestMessage::OfferRelease { .. }),
                ) => Some(message.clone()),
                _ => None,
            })
            .collect()
    }

    fn acted(&self, mark: Mark) -> Vec<GuestAction> {
        self.emitted[mark.emitted..]
            .iter()
            .filter(|action| !matches!(action, GuestAction::Send(_)))
            .cloned()
            .collect()
    }
}

async fn watch(driver: Arc<Driver<Device>>, sizes: mpsc::Sender<Event>) {
    while let Ok(event) = driver.changed().await {
        if sizes.send(event).await.is_err() {
            return;
        }
    }
}

#[tokio::test]
async fn a_guest_that_says_hello_is_welcomed_with_its_ceiling() {
    let mut harness = Harness::start().await;
    let mark = harness.mark();

    harness.handshake().await;

    let negotiated = harness.negotiated().expect("a welcomed guest");

    assert_eq!(
        negotiated.block_size.get(),
        BLOCK,
        "the guest's geometry did not reach the host"
    );

    assert_eq!(
        harness.sent(mark),
        vec![
            HostMessage::Welcome {
                protocol: PROTOCOL_VERSION,
                ceiling: CEILING,
                status_interval: Duration::from_secs(1),
                hotplugged: 0,
            },
            HostMessage::Geometry { hotplugged: 0 },
        ]
    );

    assert_eq!(harness.engine.ceiling(), Some(CEILING));
    assert_eq!(harness.size(), Some(BOOT));
    assert_eq!(
        harness.status().map(|status| status.total_mem),
        Some(BOOT),
        "the host never heard the guest's first status"
    );

    harness.tick(BUSY);

    harness
        .settle("a status carrying the workload", |harness| {
            harness
                .status()
                .is_some_and(|status| status.workload_usage == BUSY)
        })
        .await;

    assert_eq!(
        harness.status().map(|status| status.available),
        Some(BOOT - BUSY)
    );
}

#[tokio::test]
async fn a_guest_whose_device_has_said_nothing_is_not_negotiated_with_until_it_does() {
    let mut harness = Harness::unseeded(policy());

    harness.available(PLENTY).await;

    let mark = harness.mark();

    harness.lull().await;

    assert!(
        !harness.welcomed(),
        "the host welcomed a guest whose size it did not know"
    );
    assert_eq!(harness.size(), None);
    assert_eq!(
        harness.sent(mark),
        Vec::new(),
        "the host spoke before its device reported"
    );
    assert!(!harness.engine.is_connected());

    harness.seed().await;
    harness.handshake().await;

    let seeded = harness.mark();

    harness.grow(GROWN).await;

    assert_eq!(harness.size(), Some(GROWN));
    assert_eq!(harness.actuated(seeded), vec![GROWN]);
}

#[tokio::test]
async fn a_throttled_guest_is_grown_and_recomputes_its_limits() {
    let mut harness = Harness::start().await;

    harness.handshake().await;
    harness.available(PLENTY).await;

    let mark = harness.mark();

    harness.grow(GROWN).await;

    assert_eq!(harness.asked(mark), vec![request(1, BOOT, GROWN)]);
    assert_eq!(
        harness.answers(mark, 1),
        vec![HostMessage::Grant {
            id: RequestId(1),
            target: GROWN,
        }]
    );

    assert_eq!(harness.actuated(mark), vec![GROWN]);
    assert_eq!(
        harness
            .driver
            .actuator()
            .requested()
            .await
            .expect("the device answers"),
        GROWN
    );

    assert_eq!(
        harness.size(),
        Some(GROWN),
        "the new size never came back to the host"
    );
    assert_eq!(harness.requested(), None, "the actuation never converged");

    assert_eq!(
        harness.acted(mark),
        vec![
            GuestAction::SetMax(GROWN - SERVICE),
            GuestAction::SetHigh(GROWN - SERVICE - BURST),
        ]
    );

    assert_eq!(harness.engine.total_mem(), GROWN);
    assert_eq!(harness.engine.max(), GROWN - SERVICE);
    assert_eq!(harness.engine.high(), GROWN - SERVICE - BURST);
}

#[tokio::test]
async fn a_grow_the_host_cannot_afford_is_denied_and_asked_for_again() {
    let mut harness = Harness::start().await;

    harness.handshake().await;
    harness.available(FLOOR).await;

    let mark = harness.mark();

    harness.tick(BUSY);
    harness.wake();

    harness
        .settle("the denial", |harness| harness.denied(mark))
        .await;

    assert_eq!(
        harness.answers(mark, 1),
        vec![HostMessage::Deny {
            id: RequestId(1),
            reason: DenyReason::HostPressure,
            retry_after: Some(Duration::from_millis(1)),
        }]
    );
    assert_eq!(harness.actuated(mark), Vec::<u64>::new());
    assert!(matches!(harness.engine.phase(), GuestPhase::Growing(_)));
    assert_eq!(harness.engine.pending_request(), None);
    assert_eq!(harness.total_mem, BOOT);

    harness.available(PLENTY).await;

    let retry = harness.mark();

    harness.tick(BUSY);
    harness.resized(retry, GROWN).await;

    assert_eq!(
        harness.asked(mark),
        vec![request(1, BOOT, GROWN), request(2, BOOT, GROWN)]
    );
    assert_eq!(
        harness.answers(mark, 2),
        vec![HostMessage::Grant {
            id: RequestId(2),
            target: GROWN,
        }]
    );
    assert_eq!(
        harness.answers(mark, 1).len(),
        1,
        "the host answered one request twice"
    );
    assert!(matches!(harness.engine.phase(), GuestPhase::Steady(_)));
    assert_eq!(harness.engine.total_mem(), GROWN);
}

#[tokio::test]
async fn an_idle_guest_offers_memory_back_and_is_shrunk() {
    let mut harness = Harness::start().await;

    harness.handshake().await;
    harness.available(PLENTY).await;
    harness.grow(GROWN).await;

    let mark = harness.mark();

    for _ in 0..4 {
        harness.tick(CALM);
    }

    harness.resized(mark, SHRUNK).await;

    assert_eq!(
        harness.asked(mark),
        vec![GuestMessage::OfferRelease {
            id: RequestId(2),
            current: GROWN,
            releasable: RELEASE,
        }]
    );
    assert_eq!(
        harness.answers(mark, 2),
        vec![HostMessage::Grant {
            id: RequestId(2),
            target: SHRUNK,
        }]
    );

    assert_eq!(harness.actuated(mark), vec![SHRUNK]);
    assert_eq!(harness.size(), Some(SHRUNK));

    assert_eq!(
        harness.acted(mark),
        vec![
            GuestAction::StartReclaim { bytes: RELEASE },
            GuestAction::SetHigh(SHRUNK - SERVICE - BURST),
            GuestAction::SetMax(SHRUNK - SERVICE),
        ]
    );

    assert!(matches!(harness.engine.phase(), GuestPhase::Steady(_)));
    assert_eq!(harness.engine.total_mem(), SHRUNK);
    assert_eq!(harness.engine.max(), SHRUNK - SERVICE);
    assert_eq!(harness.engine.high(), SHRUNK - SERVICE - BURST);
}

#[tokio::test]
async fn a_host_under_pressure_takes_memory_back() {
    let mut harness = Harness::start_with(revoking()).await;

    harness.handshake().await;
    harness.available(PLENTY).await;
    harness.grow(GROWN).await;

    harness.tick(CALM);

    let mark = harness.mark();

    harness.pressured(PLENTY, PRESSED).await;
    harness.beat().await;

    harness.resized(mark, SHRUNK).await;

    assert_eq!(
        harness.sent(mark).first(),
        Some(&HostMessage::ReclaimRequest {
            id: ReclaimId(1),
            desired: RELEASE,
            deadline: Duration::from_secs(30),
        }),
        "the host never asked for its memory back",
    );
    assert_eq!(
        harness.emitted[mark.emitted..]
            .iter()
            .filter_map(|action| match action {
                GuestAction::Send(message @ GuestMessage::ReclaimAck { .. }) =>
                    Some(message.clone()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![GuestMessage::ReclaimAck {
            id: ReclaimId(1),
            will_release: RELEASE,
        }]
    );

    assert_eq!(
        harness.asked(mark),
        vec![GuestMessage::OfferRelease {
            id: RequestId(2),
            current: GROWN,
            releasable: RELEASE,
        }]
    );
    assert_eq!(harness.actuated(mark), vec![SHRUNK]);
    assert_eq!(harness.size(), Some(SHRUNK));
    assert_eq!(harness.engine.total_mem(), SHRUNK);
}

#[tokio::test]
async fn a_connection_that_drops_is_rebuilt_and_the_next_grow_still_works() {
    let mut harness = Harness::start().await;

    harness.handshake().await;
    harness.available(PLENTY).await;
    harness.grow(GROWN).await;

    harness.cut().await;

    assert!(!harness.welcomed(), "the host kept a session it let go of");

    harness
        .settle("the agent to notice", |harness| {
            !harness.engine.is_connected()
        })
        .await;

    harness
        .settle("the agent to come back", |harness| {
            harness.engine.is_connected() && harness.welcomed() && harness.status().is_some()
        })
        .await;

    assert_eq!(harness.accepted, 2, "the agent did not dial the host again");
    assert_eq!(
        harness.engine.ceiling(),
        Some(CEILING),
        "the second welcome lost the ceiling"
    );

    let mark = harness.mark();

    harness.grow(REGROWN).await;

    assert_eq!(harness.asked(mark), vec![request(1, GROWN, REGROWN)]);
    assert_eq!(
        harness.answers(mark, 1),
        vec![HostMessage::Grant {
            id: RequestId(1),
            target: REGROWN,
        }],
        "the host did not answer the restarted id space exactly once"
    );

    assert_eq!(harness.size(), Some(REGROWN));
    assert_eq!(harness.engine.total_mem(), REGROWN);
    assert_eq!(harness.engine.max(), REGROWN - SERVICE);
    assert_eq!(harness.engine.high(), REGROWN - SERVICE - BURST);
}
