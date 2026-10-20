pub mod error;
pub mod metrics;

use std::sync::Arc;

use pazari_metrics::Exposition;
use pazari_proto::GuestMessage;
use pazari_systemd::Notifier;
use pazari_types::Clock;
use tokio::{
    signal::unix::{SignalKind, signal},
    sync::watch,
};
use tracing::{debug, info, warn};

pub use crate::run::{error::RunError, metrics::Observer};
use crate::{
    config::Config,
    engine::{BootInfo, Engine, Event},
    runtime::{
        AnyCgroup, Applier, Cgroup, HotplugListener, Limit, Preflight, RuntimeConfig, Severity,
        Snapshots, ThrottleWatcher, Ticker, Topology,
    },
    session::{Geometry, Session},
};

pub async fn run(config: Config) -> Result<(), RunError> {
    let clock = Clock::start();

    debug!(
        engine = ?config.engine,
        runtime = ?config.runtime,
        session = ?config.session,
        "resolved settings",
    );

    let report = Preflight::new(&config.runtime).run(config.engine.reclaim_swappiness);

    if !report.passed() {
        return Err(RunError::Preflight { report });
    }

    for check in report.checks() {
        if check.severity == Severity::Warn {
            warn!(%check, "preflight warning");
        } else {
            debug!(%check, "preflight");
        }
    }

    let topology = Topology::new(&config.runtime.paths);
    let block_size = topology.block_size().await?;
    let total_mem = topology.total_mem().await?;

    let boot = BootInfo {
        block_size,
        total_mem,
    };

    info!(
        block_size = boot.block_size.get(),
        total_mem, "guest memory geometry"
    );

    let (mut engine, actions) = Engine::try_new(config.engine, boot)?;

    let (mut observer, exposition) = match config.metrics {
        Some(address) => {
            let (observer, published) = Observer::new();
            let exposition = Exposition::bind(address, published).await?;

            info!(%address, "serving metrics");

            (observer, Some(exposition))
        }
        None => (Observer::default(), None),
    };

    let cgroup = Arc::new(AnyCgroup::open(&config.runtime).await?);

    let mut applier = Applier::new(Arc::clone(&cgroup), config.runtime.reclaim_chunk, clock);

    applier
        .apply(actions, &mut |message| {
            warn!(
                ?message,
                "engine emitted a message before the session opened"
            );
        })
        .await?;

    let mut ticker = Ticker::new(
        Snapshots::new(Arc::clone(&cgroup), topology.clone()),
        config.runtime.tick_interval,
        clock,
    );
    let mut throttle = ThrottleWatcher::open(&config.runtime.cgroup, clock)?;
    let hotplug = HotplugListener::bind(topology, config.runtime.hotplug_debounce, clock)?;

    let (total_mem, latest) = watch::channel(total_mem);

    let (mut session, outbound) = Session::spawn(
        config.session,
        Geometry {
            block_size,
            total_mem: latest,
        },
        clock,
    );

    let mut terminate = signal(SignalKind::terminate()).map_err(|source| RunError::Signal {
        signal: "SIGTERM",
        source,
    })?;
    let mut interrupt = signal(SignalKind::interrupt()).map_err(|source| RunError::Signal {
        signal: "SIGINT",
        source,
    })?;

    let endpoint = exposition.map(|exposition| tokio::spawn(exposition.serve()));

    let mut notifier = Notifier::new();
    notifier.ready();

    info!("agent started");

    loop {
        let step = tokio::select! {
            tick = ticker.tick() => Step::Event(tick?),
            wake = throttle.wake() => Step::Event(wake?),
            changed = hotplug.changed() => Step::Event(changed?),
            finished = applier.reclaimer().finished() => Step::Event(finished?),
            event = session.event() => Step::Event(event.ok_or(RunError::SessionLost)?),
            () = notifier.due() => Step::Ping,
            _ = terminate.recv() => Step::Stop,
            _ = interrupt.recv() => Step::Stop,
        };

        let event = match step {
            Step::Event(event) => event,
            Step::Ping => {
                notifier.ping();
                continue;
            }
            Step::Stop => break,
        };

        observer.observe_event(&event);

        let actions = engine.handle(event);

        observer.refresh(&engine);
        observer.observe_actions(&actions);

        total_mem.send_replace(engine.total_mem());

        applier
            .apply(actions, &mut |message| outbound.send(message))
            .await?;
    }

    info!("stopping");
    notifier.stopping();

    outbound.send(GuestMessage::Bye {});
    session.shutdown(outbound).await;
    cgroup.set_high(Limit::Max).await?;

    if let Some(endpoint) = endpoint {
        endpoint.abort();
    }

    Ok(())
}

pub async fn panic_open(config: &RuntimeConfig) -> Result<(), RunError> {
    let cgroup = AnyCgroup::open(config).await?;

    cgroup.set_high(Limit::Max).await?;

    println!("{}", cgroup.released());

    Ok(())
}

#[derive(Debug)]
enum Step {
    Event(Event),

    Ping,

    Stop,
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        num::{NonZeroU32, NonZeroU64},
        path::Path,
        time::Duration,
    };

    use pazari_proto::{Host, HostMessage, JsonLinesCodec, PROTOCOL_VERSION, Status};
    use pazari_transport::{AnyConnector, Listener, MessageStream, UnixConnector, UnixListener};
    use pazari_types::{Delay, Interval};
    use tempfile::TempDir;
    use tokio::{net::UnixStream, time::timeout};

    use super::*;
    use crate::{
        engine::{
            EngineConfig, ExhaustionMode, ExponentialConfig, GrowConfig, ReleaseConfig, Snapshot,
            ThrottleMode, TitrateConfig,
        },
        runtime::{cgroup::MEMORY_HIGH, testing::cgroup_dir},
        session::SessionConfig,
    };

    const MIB: u64 = 1 << 20;
    const BOOT: u64 = 2048 * MIB;
    const BLOCK: u64 = 128 * MIB;
    const SERVICE: u64 = 256 * MIB;
    const BURST: u64 = 256 * MIB;
    const CEILING: u64 = 4096 * MIB;

    const PATIENCE: Duration = Duration::from_secs(5);

    type HostStream = MessageStream<UnixStream, JsonLinesCodec, Host>;

    fn bytes(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).expect("a positive byte count")
    }

    fn interval(secs: u64) -> Interval {
        Interval::try_new(Duration::from_secs(secs)).expect("a positive interval under a day")
    }

    fn engine_config() -> EngineConfig {
        EngineConfig {
            service_reserve: SERVICE,
            burst_reserve: bytes(BURST),
            throttle: ThrottleMode::Grow,
            grow: GrowConfig::Exponential(ExponentialConfig {
                step: bytes(512 * MIB),
                escalate_within: interval(10),
                cap: bytes(64),
            }),
            release: ReleaseConfig::Titrate(TitrateConfig {
                step: bytes(256 * MIB),
                after: NonZeroU32::new(3).expect("a positive count"),
                cooldown: Delay::try_new(Duration::from_secs(30)).expect("a short delay"),
            }),
            deny_backoff: interval(5),
            settle_grace: interval(10),
            recover_after: NonZeroU32::new(3).expect("a positive count"),
            reclaim_swappiness: None,
            exhaustion: ExhaustionMode::Kill,
        }
    }

    fn read(dir: &Path, name: &str) -> String {
        fs::read_to_string(dir.join(name))
            .expect("a cgroup file")
            .trim()
            .to_owned()
    }

    async fn hear(host: &mut HostStream) -> GuestMessage {
        timeout(PATIENCE, host.recv())
            .await
            .expect("the agent spoke in time")
            .expect("a readable connection")
            .expect("a message")
    }

    #[tokio::test]
    async fn the_brake_is_released_on_whatever_cgroup_the_file_named() {
        let dir = cgroup_dir();
        let config = RuntimeConfig::new(dir.path());

        fs::write(dir.path().join(MEMORY_HIGH), "1073741824\n").expect("a starting limit");

        panic_open(&config).await.expect("the brake comes off");

        assert_eq!(read(dir.path(), MEMORY_HIGH), "max");
    }

    #[tokio::test]
    async fn releasing_a_brake_that_is_already_off_is_still_a_release() {
        let dir = cgroup_dir();
        let config = RuntimeConfig::new(dir.path());

        panic_open(&config).await.expect("the brake comes off");
        panic_open(&config).await.expect("and stays off");

        assert_eq!(read(dir.path(), MEMORY_HIGH), "max");
    }

    #[tokio::test]
    async fn a_cgroup_that_is_not_there_is_reported() {
        let dir = TempDir::new().expect("a temporary directory");
        let config = RuntimeConfig::new(dir.path().join("absent"));

        let error = panic_open(&config)
            .await
            .expect_err("there is nothing to write");

        assert!(
            error.to_string().contains("memory.high"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn the_engine_actuates_the_cgroup_and_talks_to_the_host() {
        let clock = Clock::start();
        let dir = cgroup_dir();
        let socket = TempDir::new().expect("a temporary directory");
        let path = socket.path().join("host.sock");

        let listener = UnixListener::bind(&path).expect("a bound listener");

        let runtime = RuntimeConfig::new(dir.path());
        let cgroup = Arc::new(AnyCgroup::open(&runtime).await.expect("a cgroupfs adapter"));

        let (mut engine, actions) = Engine::try_new(
            engine_config(),
            BootInfo {
                block_size: bytes(BLOCK),
                total_mem: BOOT,
            },
        )
        .expect("a workable boot");

        let mut applier = Applier::new(Arc::clone(&cgroup), runtime.reclaim_chunk, clock);

        applier
            .apply(actions, &mut |_| panic!("the engine is not connected yet"))
            .await
            .expect("the ground state lands");

        assert_eq!(read(dir.path(), "memory.max"), (BOOT - SERVICE).to_string());
        assert_eq!(
            read(dir.path(), "memory.high"),
            (BOOT - SERVICE - BURST).to_string()
        );
        assert_eq!(read(dir.path(), "memory.oom.group"), "1");

        let (total_mem, latest) = watch::channel(BOOT);

        let (mut session, outbound) = Session::spawn(
            SessionConfig {
                connector: AnyConnector::Unix(UnixConnector::new(&path)),
                handshake_timeout: interval(5),
                reconnect_min: Interval::try_new(Duration::from_millis(1)).expect("a tick"),
                reconnect_max: Interval::try_new(Duration::from_millis(5)).expect("a tick"),
            },
            Geometry {
                block_size: bytes(BLOCK),
                total_mem: latest,
            },
            clock,
        );

        let (connection, _) = timeout(PATIENCE, listener.accept())
            .await
            .expect("the agent dialled in time")
            .expect("a connection");

        let mut host = HostStream::new(connection);

        hear(&mut host).await;

        host.send(&HostMessage::Welcome {
            protocol: PROTOCOL_VERSION,
            ceiling: CEILING,
            status_interval: Duration::from_millis(1),
            hotplugged: 0,
        })
        .await
        .expect("a writable connection");

        let drive = async |event: Event, engine: &mut Engine, applier: &mut Applier<_>| {
            let actions = engine.handle(event);
            total_mem.send_replace(engine.total_mem());
            applier
                .apply(actions, &mut |message| outbound.send(message))
                .await
                .expect("the actions land");
        };

        drive(
            Event::Tick {
                now: clock.now(),
                snapshot: Snapshot {
                    total_mem: BOOT,
                    workload_usage: 1200 * MIB,
                    free: 0,
                    available: BOOT - 1200 * MIB,
                    swap_used: 0,
                    high_events: 0,
                    max_events: 0,
                    psi_some10: 0.0,
                    psi_full10: 0.0,
                },
            },
            &mut engine,
            &mut applier,
        )
        .await;

        let connected = timeout(PATIENCE, session.event())
            .await
            .expect("the session answered in time")
            .expect("the session is still running");

        assert!(matches!(connected, Event::Connected { .. }));
        drive(connected, &mut engine, &mut applier).await;

        assert!(
            matches!(
                hear(&mut host).await,
                GuestMessage::Status(Status {
                    total_mem: BOOT,
                    ..
                })
            ),
            "the engine did not greet the host with its state"
        );

        drive(
            Event::ThrottleWake {
                now: clock.now(),
                high_events: 1,
                max_events: 0,
            },
            &mut engine,
            &mut applier,
        )
        .await;

        assert_eq!(
            read(dir.path(), "memory.high"),
            (BOOT - SERVICE - BURST).to_string(),
            "the brake moved when the workload was throttled"
        );

        let mut asked = None;
        for _ in 0..8 {
            if let GuestMessage::GrantRequest { target, .. } = hear(&mut host).await {
                asked = Some(target);
                break;
            }
        }

        assert_eq!(
            asked,
            Some(BOOT + 512 * MIB),
            "the engine did not ask the host for more memory"
        );

        drop(host);
        session.shutdown(outbound).await;
    }
}
