pub mod error;
pub mod exchange;
pub mod host;
pub mod metrics;
pub mod peers;
pub mod supervisor;

use pazari_metrics::Exposition;
use pazari_systemd::Notifier;
use pazari_transport::{AnyConnection, Listener, PeerId, TransportError};
use pazari_types::{Backoff, Clock};
use tokio::{
    signal::unix::{SignalKind, signal},
    sync::{mpsc, watch},
    time::{self, MissedTickBehavior},
};
use tracing::{info, warn};

pub use crate::run::{
    error::RunError,
    exchange::{Exchange, Target},
    host::Demand,
    metrics::{DeviceMetrics, Observer},
    peers::{Peers, Report},
    supervisor::{Connect, Supervisor},
};
use crate::{
    broker::{Broker, Event},
    config::Config,
    runtime::ProcMemory,
};

const SIZE_DEPTH: usize = 64;

const ACCEPT_DEPTH: usize = 64;

#[derive(Debug)]
enum Step {
    Accepted(Result<(AnyConnection, PeerId), TransportError>),

    Reported(Report),

    Sized(Event),

    Tick,

    Ping,

    Stop,
}

pub async fn run(config: Config) -> Result<(), RunError> {
    let clock = Clock::start();

    let Config {
        tick_interval,
        listeners,
        metrics,
        policy,
        registry,
        devices,
    } = config;

    let memory = ProcMemory::new();

    host::survey(&memory, &policy, &registry).await?;

    let (observer, exposition) = match metrics {
        Some(address) => {
            let (observer, published) = Observer::new();
            let exposition = Exposition::bind(address, published).await?;

            info!(%address, "serving metrics");

            (observer, Some(exposition))
        }
        None => (Observer::default(), None),
    };

    let (sizes, mut resized) = mpsc::channel(SIZE_DEPTH);
    let mut targets = Vec::with_capacity(devices.len());
    let mut supervisors = Vec::with_capacity(devices.len());

    for ((guest, spec), qmp) in registry.iter().zip(devices) {
        let (target, targeted) = watch::channel(None);

        info!(
            guest = spec.name.as_str(),
            device = qmp.device.as_str(),
            ceiling = spec.ceiling.get(),
            "registering a guest",
        );

        let backoff = Backoff::new(qmp.retry_min.get(), qmp.retry_max.get());

        supervisors.push(tokio::spawn(
            Supervisor::new(
                guest,
                spec.name.clone(),
                qmp,
                clock,
                sizes.clone(),
                targeted,
                backoff,
                observer.device(&spec.name),
            )
            .run(),
        ));

        targets.push(target);
    }

    drop(sizes);

    let (connections, mut accepted) = mpsc::channel(ACCEPT_DEPTH);
    let mut acceptors = Vec::with_capacity(listeners.len());

    for transport in listeners {
        let listener = transport.bind()?;
        let connections = connections.clone();

        info!(local = %listener.local(), "listening");

        acceptors.push(tokio::spawn(async move {
            loop {
                if connections.send(listener.accept().await).await.is_err() {
                    return;
                }
            }
        }));
    }

    drop(connections);

    let endpoint = exposition.map(|exposition| tokio::spawn(exposition.serve()));

    let (peers, mut reports) = Peers::new();

    let mut exchange = Exchange {
        broker: Broker::new(policy, registry),
        peers,
        targets,
        observer,
    };

    let mut ticker = time::interval(tick_interval.get());
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    let mut terminate = signal(SignalKind::terminate()).map_err(|source| RunError::Signal {
        signal: "SIGTERM",
        source,
    })?;
    let mut interrupt = signal(SignalKind::interrupt()).map_err(|source| RunError::Signal {
        signal: "SIGINT",
        source,
    })?;

    let mut notifier = Notifier::new();
    notifier.ready();

    info!("daemon started");

    loop {
        let step = tokio::select! {
            connection = accepted.recv() => connection.map_or(Step::Stop, Step::Accepted),
            reported = reports.recv() => reported.map_or(Step::Stop, Step::Reported),
            sized = resized.recv() => sized.map_or(Step::Stop, Step::Sized),
            _ = ticker.tick() => Step::Tick,
            () = notifier.due() => Step::Ping,
            _ = terminate.recv() => Step::Stop,
            _ = interrupt.recv() => Step::Stop,
        };

        match step {
            Step::Accepted(Ok((connection, peer))) => {
                exchange.peers.accept(connection, peer.clone());

                exchange.drive(Event::Connected { peer });
            }
            Step::Accepted(Err(error)) => warn!(%error, "failed to accept a connection"),
            Step::Reported(report) => {
                if let Some(event) = exchange.peers.digest(report, clock.now()) {
                    exchange.drive(event);
                }
            }
            Step::Sized(sized) => exchange.drive(sized),
            Step::Tick => {
                let now = clock.now();

                match host::reading(&memory).await {
                    Ok(event) => exchange.drive(event),
                    Err(error) => warn!(%error, "failed to read host memory"),
                }

                exchange.drive(Event::Tick { now });
                exchange.refresh();
            }
            Step::Ping => notifier.ping(),
            Step::Stop => break,
        }
    }

    info!("stopping");
    notifier.stopping();

    exchange.peers.clear();

    for acceptor in acceptors {
        acceptor.abort();
    }

    for supervisor in supervisors {
        supervisor.abort();
    }

    if let Some(endpoint) = endpoint {
        endpoint.abort();
    }

    Ok(())
}
