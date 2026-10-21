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
        AnyCgroup, Applier, Cgroup, HotplugListener, Limit, Preflight, RuntimeConfig, RuntimeError,
        Severity, Snapshots, ThrottleWatcher, Ticker, Topology,
    },
    session::{Geometry, Outbound, Session},
};

pub async fn run(config: Config) -> Result<(), RunError> {
    let clock = Clock::start();

    debug!(
        engine = ?config.engine,
        runtime = ?config.runtime,
        session = ?config.session,
        "resolved settings",
    );

    let report = Preflight::new(&config.runtime).run();

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

    let (engine, ground) = Engine::try_new(config.engine, boot)?;

    let (observer, exposition) = match config.metrics {
        Some(address) => {
            let (observer, published) = Observer::new();
            let exposition = Exposition::bind(address, published).await?;

            info!(%address, "serving metrics");

            (observer, Some(exposition))
        }
        None => (Observer::default(), None),
    };

    let cgroup = Arc::new(AnyCgroup::open(&config.runtime).await?);

    let applier = Applier::new(Arc::clone(&cgroup), config.runtime.reclaim, clock);

    applier.ground(ground).await?;

    let mut ticker = Ticker::new(
        Snapshots::new(Arc::clone(&cgroup), topology.clone()),
        config.runtime.tick_interval,
        clock,
    );
    let mut throttle = ThrottleWatcher::open(&config.runtime.cgroup, clock)?;
    let mut hotplug = HotplugListener::bind(topology, config.runtime.hotplug_debounce, clock)?;

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

    let mut agent = Agent {
        engine,
        observer,
        applier,
        total_mem,
        outbound,
    };

    loop {
        let step = tokio::select! {
            tick = ticker.tick() => Step::Event(tick?),
            wake = throttle.wake() => Step::Event(wake?),
            changed = hotplug.changed() => Step::Event(changed?),
            finished = agent.applier.reclaimer().finished() => Step::Event(finished?),
            event = session.event() => Step::Event(event.ok_or(RunError::SessionLost)?),
            () = notifier.due() => Step::Ping,
            _ = terminate.recv() => Step::Stop,
            _ = interrupt.recv() => Step::Stop,
        };

        match step {
            Step::Event(event) => agent.handle(event).await?,
            Step::Ping => notifier.ping(),
            Step::Stop => break,
        }
    }

    info!("stopping");
    notifier.stopping();

    agent.outbound.send(GuestMessage::Bye {});
    session.shutdown(agent.outbound).await;
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

/// Passes each event to the engine and carries out what it decides.
#[derive(Debug)]
struct Agent<C> {
    engine: Engine,
    observer: Observer,
    applier: Applier<C>,
    total_mem: watch::Sender<u64>,
    outbound: Outbound,
}

impl<C: Cgroup + 'static> Agent<C> {
    async fn handle(&mut self, event: Event) -> Result<(), RuntimeError> {
        self.observer.observe_event(&event);

        let outcome = self.engine.handle(event);

        self.observer.refresh(&self.engine);
        self.observer.observe_outcome(&outcome);

        self.total_mem.send_replace(self.engine.total_mem());

        let outbound = &self.outbound;

        self.applier
            .apply(outcome.actions, &mut |message| outbound.send(message))
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, time::Duration};

    use pazari_proto::{Host, HostMessage, JsonLinesCodec, PROTOCOL_VERSION, Status};
    use pazari_transport::{AnyConnector, Listener, MessageStream, UnixConnector, UnixListener};
    use tempfile::TempDir;
    use tokio::{net::UnixStream, time::timeout};

    use super::*;
    use crate::{
        engine::{
            EngineConfig, ExhaustionMode,
            testing::{
                BOOT, BURST, BUSY, CEILING, MIB, SERVICE, boot, config, interval, interval_ms, snap,
            },
        },
        runtime::{
            cgroup::{MEMORY_HIGH, MEMORY_MAX, MEMORY_OOM_GROUP},
            testing::{cgroup_dir, runtime_config},
        },
        session::SessionConfig,
    };

    const PATIENCE: Duration = Duration::from_secs(5);

    type HostStream = MessageStream<UnixStream, JsonLinesCodec, Host>;

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
        let config = runtime_config(dir.path());

        fs::write(dir.path().join(MEMORY_HIGH), "1073741824\n").expect("a starting limit");

        panic_open(&config).await.expect("the brake comes off");

        assert_eq!(read(dir.path(), MEMORY_HIGH), "max");
    }

    #[tokio::test]
    async fn a_cgroup_that_is_not_there_is_reported() {
        let dir = TempDir::new().expect("a temporary directory");
        let config = runtime_config(&dir.path().join("absent"));

        let error = panic_open(&config)
            .await
            .expect_err("there is nothing to write");

        assert!(
            error.to_string().contains(MEMORY_HIGH),
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

        let runtime = runtime_config(dir.path());
        let cgroup = Arc::new(AnyCgroup::open(&runtime).await.expect("a cgroupfs adapter"));

        let (engine, ground) = Engine::try_new(
            EngineConfig {
                exhaustion: ExhaustionMode::Kill,
                ..config()
            },
            boot(),
        )
        .expect("a workable boot");

        let applier = Applier::new(Arc::clone(&cgroup), runtime.reclaim, clock);

        applier
            .ground(ground)
            .await
            .expect("the ground state lands");

        assert_eq!(read(dir.path(), MEMORY_MAX), (BOOT - SERVICE).to_string());
        assert_eq!(
            read(dir.path(), MEMORY_HIGH),
            (BOOT - SERVICE - BURST).to_string()
        );
        assert_eq!(read(dir.path(), MEMORY_OOM_GROUP), "1");

        let (total_mem, latest) = watch::channel(BOOT);

        let (mut session, outbound) = Session::spawn(
            SessionConfig {
                handshake_timeout: interval(5),
                reconnect_min: interval_ms(1),
                reconnect_max: interval_ms(5),
                connector: AnyConnector::Unix(UnixConnector::new(&path)),
            },
            Geometry {
                block_size: boot().block_size,
                total_mem: latest,
            },
            clock,
        );

        let mut agent = Agent {
            engine,
            observer: Observer::default(),
            applier,
            total_mem,
            outbound,
        };

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

        agent
            .handle(Event::Tick {
                now: clock.now(),
                snapshot: snap(BOOT, BUSY, 0),
            })
            .await
            .expect("the tick lands");

        let connected = timeout(PATIENCE, session.event())
            .await
            .expect("the session answered in time")
            .expect("the session is still running");

        assert!(matches!(connected, Event::Connected { .. }));
        agent.handle(connected).await.expect("the connect lands");

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

        agent
            .handle(Event::ThrottleWake {
                now: clock.now(),
                high_events: 1,
                max_events: 0,
            })
            .await
            .expect("the wake lands");

        assert_eq!(
            read(dir.path(), MEMORY_HIGH),
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
        session.shutdown(agent.outbound).await;
    }
}
