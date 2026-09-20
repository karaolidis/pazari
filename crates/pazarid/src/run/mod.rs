pub mod error;
pub mod peers;

use std::sync::Arc;

use pazari_systemd::Notifier;
use pazari_transport::{AnyConnection, AnyListener, Listener, PeerId, TransportError};
use pazari_types::Clock;
use tokio::{
    signal::unix::{SignalKind, signal},
    sync::mpsc,
    time::{self, MissedTickBehavior},
};
use tracing::{info, warn};

pub use crate::run::{
    error::RunError,
    peers::{Peers, Report},
};
use crate::{
    broker::{Action, Broker, Event},
    config::Config,
    runtime::{Driver, HostMemory, ProcMemory, QmpActuator, RuntimeError},
};

const SIZE_DEPTH: usize = 64;

const ACCEPT_DEPTH: usize = 64;

type Device = Arc<Driver<QmpActuator>>;

#[derive(Debug)]
enum Step {
    Accepted(Result<(AnyConnection, PeerId), TransportError>),

    Reported(Report),

    Sized(Result<Event, RuntimeError>),

    Tick,

    Ping,

    Stop,
}

pub async fn run(config: Config) -> Result<(), RunError> {
    let clock = Clock::start();

    let Config {
        tick_interval,
        listeners,
        policy,
        registry,
        devices,
    } = config;

    let (connections, mut accepted) = mpsc::channel(ACCEPT_DEPTH);
    let mut acceptors = Vec::with_capacity(listeners.len());

    for transport in listeners {
        let listener = transport.bind()?;

        info!(local = %listener.local(), "the daemon is listening");

        acceptors.push(tokio::spawn(serve(listener, connections.clone())));
    }

    drop(connections);

    let mut drivers: Vec<Device> = Vec::with_capacity(devices.len());

    for ((guest, spec), qmp) in registry.iter().zip(devices) {
        let actuator = QmpActuator::connect(qmp.path, qmp.device, qmp.mode, qmp.deadline).await?;

        info!(
            guest = spec.name.as_str(),
            device = actuator.device(),
            boot_mem = actuator.boot(),
            ceiling = spec.ceiling,
            "the guest's memory device",
        );

        drivers.push(Arc::new(Driver::new(guest, actuator, clock)));
    }

    let mut broker = Broker::new(policy, registry);
    let memory = ProcMemory::new();

    let (mut peers, mut reports) = Peers::new();
    let (sizes, mut resized) = mpsc::channel(SIZE_DEPTH);
    let mut watchers = Vec::with_capacity(drivers.len());

    for driver in &drivers {
        let seed = driver.seed().await?;

        drive(&mut broker, &mut peers, &drivers, seed).await?;

        watchers.push(tokio::spawn(watch(Arc::clone(driver), sizes.clone())));
    }

    drop(sizes);

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

    info!("the daemon is running");

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
                peers.accept(connection, peer.clone());

                let connected = Event::Connected {
                    now: clock.now(),
                    peer,
                };

                drive(&mut broker, &mut peers, &drivers, connected).await?;
            }
            Step::Accepted(Err(error)) => warn!(%error, "failed to take on a connection"),
            Step::Reported(report) => {
                if let Some(event) = peers.digest(report, clock.now()) {
                    drive(&mut broker, &mut peers, &drivers, event).await?;
                }
            }
            Step::Sized(sized) => drive(&mut broker, &mut peers, &drivers, sized?).await?,
            Step::Tick => {
                let now = clock.now();

                match memory.available().await {
                    Ok(bytes) => {
                        drive(
                            &mut broker,
                            &mut peers,
                            &drivers,
                            Event::HostAvailable { now, bytes },
                        )
                        .await?;
                    }
                    Err(error) => warn!(%error, "failed to read the host's memory"),
                }

                drive(&mut broker, &mut peers, &drivers, Event::Tick { now }).await?;
            }
            Step::Ping => notifier.ping(),
            Step::Stop => break,
        }
    }

    info!("stopping");
    notifier.stopping();

    peers.clear();

    for acceptor in acceptors {
        acceptor.abort();
    }

    for watcher in watchers {
        watcher.abort();
    }

    Ok(())
}

async fn serve(
    listener: AnyListener,
    connections: mpsc::Sender<Result<(AnyConnection, PeerId), TransportError>>,
) {
    loop {
        if connections.send(listener.accept().await).await.is_err() {
            return;
        }
    }
}

async fn drive(
    broker: &mut Broker,
    peers: &mut Peers,
    drivers: &[Device],
    event: Event,
) -> Result<(), RunError> {
    let actions = broker.handle(event);

    for action in &actions {
        match action {
            Action::Send { peer, message } => peers.send(peer, message.clone()),
            Action::Close { peer } => peers.close(peer),
            Action::SetRequested { .. } => {}
        }
    }

    for driver in drivers {
        driver.apply(&actions).await?;
    }

    Ok(())
}

async fn watch(driver: Device, sizes: mpsc::Sender<Result<Event, RuntimeError>>) {
    loop {
        let changed = driver.changed().await;
        let failed = changed.is_err();

        if sizes.send(changed).await.is_err() || failed {
            return;
        }
    }
}
