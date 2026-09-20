pub mod error;
pub mod host;
pub mod peers;
pub mod supervisor;

use pazari_systemd::Notifier;
use pazari_transport::{AnyConnection, AnyListener, Listener, PeerId, TransportError};
use pazari_types::{Backoff, Clock, Timestamp};
use tokio::{
    signal::unix::{SignalKind, signal},
    sync::{mpsc, watch},
    time::{self, MissedTickBehavior},
};
use tracing::{info, warn};

pub use crate::run::{
    error::RunError,
    host::Demand,
    peers::{Peers, Report},
    supervisor::{Connect, Supervisor},
};
use crate::{
    broker::{Action, Broker, Event},
    config::Config,
    runtime::{HostMemory, ProcMemory, RuntimeError},
};

const SIZE_DEPTH: usize = 64;

const ACCEPT_DEPTH: usize = 64;

type Target = watch::Sender<Option<u64>>;

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
        policy,
        registry,
        devices,
    } = config;

    let memory = ProcMemory::new();

    host::survey(&memory, &policy, &registry).await?;

    let (sizes, mut resized) = mpsc::channel(SIZE_DEPTH);
    let mut targets = Vec::with_capacity(devices.len());
    let mut supervisors = Vec::with_capacity(devices.len());

    for ((guest, spec), qmp) in registry.iter().zip(devices) {
        let (target, targeted) = watch::channel(None);

        info!(
            guest = spec.name.as_str(),
            device = qmp.device.as_str(),
            ceiling = spec.ceiling,
            "the daemon is taking a guest on",
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

        info!(local = %listener.local(), "the daemon is listening");

        acceptors.push(tokio::spawn(serve(listener, connections.clone())));
    }

    drop(connections);

    let mut broker = Broker::new(policy, registry);
    let (mut peers, mut reports) = Peers::new();

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

                dispatch(&mut peers, &targets, &broker.handle(connected));
            }
            Step::Accepted(Err(error)) => warn!(%error, "failed to take on a connection"),
            Step::Reported(report) => {
                if let Some(event) = peers.digest(report, clock.now()) {
                    dispatch(&mut peers, &targets, &broker.handle(event));
                }
            }
            Step::Sized(sized) => dispatch(&mut peers, &targets, &broker.handle(sized)),
            Step::Tick => {
                let now = clock.now();

                match reading(&memory, now).await {
                    Ok(event) => dispatch(&mut peers, &targets, &broker.handle(event)),
                    Err(error) => warn!(%error, "failed to read the host's memory"),
                }

                dispatch(&mut peers, &targets, &broker.handle(Event::Tick { now }));
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

    for supervisor in supervisors {
        supervisor.abort();
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

async fn reading<M: HostMemory>(memory: &M, now: Timestamp) -> Result<Event, RuntimeError> {
    let info = memory.meminfo().await?;
    let pressure = memory.pressure().await?;

    Ok(Event::HostAvailable {
        now,
        bytes: info.available,
        swap_free: info.swap_free,
        psi: pressure.map(|pressure| pressure.some10),
    })
}

fn dispatch(peers: &mut Peers, targets: &[Target], actions: &[Action]) {
    for action in actions {
        match action {
            Action::Send { peer, message } => peers.send(peer, message.clone()),
            Action::Close { peer } => peers.close(peer),
            Action::SetRequested { guest, bytes } => {
                if let Some(target) = targets.get(guest.index()) {
                    target.send_replace(Some(*bytes));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use pazari_proto::HostMessage;

    use super::*;
    use crate::{
        broker::{
            GuestId,
            testing::{BOOT, MIB, ONE, vsock},
        },
        runtime::testing::{MockMemory, meminfo},
    };

    #[tokio::test]
    async fn a_reading_carries_the_whole_host_at_one_instant() {
        let memory = MockMemory::new(meminfo(8 << 30, 4 << 30, 2 << 30)).pressured(12.5);

        let event = reading(&memory, Timestamp::START)
            .await
            .expect("the mock answers");

        let Event::HostAvailable {
            bytes,
            swap_free,
            psi,
            ..
        } = event
        else {
            panic!("the daemon read {event:?}");
        };

        assert_eq!((bytes, swap_free), (4 << 30, 2 << 30));
        assert_eq!(psi, Some(12.5));
    }

    #[tokio::test]
    async fn a_host_without_pressure_accounting_reads_as_much_as_it_can() {
        let memory = MockMemory::new(meminfo(8 << 30, 4 << 30, 0));

        let event = reading(&memory, Timestamp::START)
            .await
            .expect("the mock answers");

        assert!(matches!(event, Event::HostAvailable { psi: None, .. }));
    }

    #[test]
    fn one_guest_that_cannot_be_actuated_does_not_swallow_the_batch() {
        let (mut peers, _reports) = Peers::new();

        let (one, deaf) = watch::channel(None);
        let (two, mut listening) = watch::channel(None);

        drop(deaf);

        dispatch(
            &mut peers,
            &[one, two],
            &[
                Action::SetRequested {
                    guest: GuestId::from_index(0),
                    bytes: BOOT,
                },
                Action::Send {
                    peer: vsock(ONE),
                    message: HostMessage::Ceiling { ceiling: BOOT },
                },
                Action::SetRequested {
                    guest: GuestId::from_index(1),
                    bytes: 3072 * MIB,
                },
                Action::Close { peer: vsock(ONE) },
            ],
        );

        assert_eq!(*listening.borrow_and_update(), Some(3072 * MIB));
    }

    #[test]
    fn a_size_for_a_guest_with_no_device_is_let_go_of() {
        let (mut peers, _reports) = Peers::new();

        dispatch(
            &mut peers,
            &[],
            &[Action::SetRequested {
                guest: GuestId::from_index(7),
                bytes: BOOT,
            }],
        );
    }
}
