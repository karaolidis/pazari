use std::{num::NonZeroU64, time::Duration};

use pazari_proto::{GuestMessage, HostMessage, PROTOCOL_VERSION};
use pazari_transport::{
    self as transport, AnyConnection, AnyConnector, Connector, Ended, GuestStream,
};
use pazari_types::{Backoff, Clock, Interval};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time::{sleep, timeout},
};
use tracing::{Instrument, debug, info, info_span, warn};

use crate::engine::Event;

const EVENT_DEPTH: usize = 64;
const OUTBOUND_DEPTH: usize = 64;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

type Stream = GuestStream<AnyConnection>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionConfig {
    pub handshake_timeout: Interval,

    pub reconnect_min: Interval,
    pub reconnect_max: Interval,

    pub connector: AnyConnector,
}

#[derive(Debug)]
pub struct Geometry {
    pub block_size: NonZeroU64,
    pub total_mem: watch::Receiver<u64>,
}

impl Geometry {
    fn hello(&self) -> GuestMessage {
        GuestMessage::Hello {
            protocol: PROTOCOL_VERSION,
            agent_version: env!("CARGO_PKG_VERSION").to_owned(),
            block_size: self.block_size,
            total_mem: *self.total_mem.borrow(),
        }
    }
}

#[derive(Debug)]
pub struct Session {
    events: mpsc::Receiver<Event>,
    task: JoinHandle<()>,
}

impl Session {
    #[must_use]
    pub fn spawn(config: SessionConfig, geometry: Geometry, clock: Clock) -> (Self, Outbound) {
        let (reporter, events) = mpsc::channel(EVENT_DEPTH);
        let (messages, queue) = mpsc::channel(OUTBOUND_DEPTH);

        let task = tokio::spawn(run(config, geometry, clock, reporter, queue));

        (Self { events, task }, Outbound { messages })
    }

    pub async fn event(&mut self) -> Option<Event> {
        self.events.recv().await
    }

    pub async fn shutdown(self, outbound: Outbound) {
        drop(outbound);

        if timeout(SHUTDOWN_GRACE, self.task).await.is_err() {
            warn!("session did not close in time; abandoning it");
        }
    }
}

#[derive(Debug, Clone)]
pub struct Outbound {
    messages: mpsc::Sender<GuestMessage>,
}

impl Outbound {
    pub fn send(&self, message: GuestMessage) {
        match self.messages.try_send(message) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(message)) => {
                warn!(?message, "session queue is full; dropping a message");
            }
            Err(mpsc::error::TrySendError::Closed(message)) => {
                warn!(?message, "session is closed; dropping a message");
            }
        }
    }
}

/// What the host's `welcome` told the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Welcome {
    ceiling: u64,
    status_interval: Duration,
    hotplugged: u64,
}

/// Why a connection ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Retry,

    /// The agent is shutting down, or the main loop is gone.
    Closed,
}

async fn run(
    config: SessionConfig,
    geometry: Geometry,
    clock: Clock,
    events: mpsc::Sender<Event>,
    mut outgoing: mpsc::Receiver<GuestMessage>,
) {
    let mut backoff = Backoff::new(config.reconnect_min.get(), config.reconnect_max.get());

    loop {
        let span = info_span!("connection", peer = %config.connector.peer());

        let outcome = attempt(
            &config,
            &geometry,
            clock,
            &events,
            &mut outgoing,
            &mut backoff,
        )
        .instrument(span)
        .await;

        if outcome == Outcome::Closed {
            return;
        }

        let delay = backoff.delay();

        debug!(?delay, "reconnecting");
        sleep(delay).await;
    }
}

async fn attempt(
    config: &SessionConfig,
    geometry: &Geometry,
    clock: Clock,
    events: &mpsc::Sender<Event>,
    outgoing: &mut mpsc::Receiver<GuestMessage>,
    backoff: &mut Backoff,
) -> Outcome {
    let connection = match config.connector.connect().await {
        Ok(connection) => connection,
        Err(error) => {
            warn!(%error, "failed to connect to the host");
            return Outcome::Retry;
        }
    };

    let mut stream = Stream::new(connection);

    if let Err(error) = stream.send(&geometry.hello()).await {
        warn!(%error, "failed to send hello");
        return Outcome::Retry;
    }

    let Some(Welcome {
        ceiling,
        status_interval,
        hotplugged,
    }) = welcome(&mut stream, config.handshake_timeout.get()).await
    else {
        return Outcome::Retry;
    };

    backoff.reset();

    while outgoing.try_recv().is_ok() {}

    info!(ceiling, ?status_interval, hotplugged, "host sent welcome");

    let connected = Event::Connected {
        now: clock.now(),
        ceiling,
        status_interval,
        hotplugged,
    };

    if events.send(connected).await.is_err() {
        return Outcome::Closed;
    }

    let outcome = pump(&mut stream, events, outgoing, clock).await;

    if outcome == Outcome::Retry
        && events
            .send(Event::Disconnected { now: clock.now() })
            .await
            .is_err()
    {
        return Outcome::Closed;
    }

    outcome
}

async fn welcome(stream: &mut Stream, within: Duration) -> Option<Welcome> {
    let first = match timeout(within, stream.recv()).await {
        Ok(Ok(Some(message))) => message,
        Ok(Ok(None)) => {
            warn!("host closed the connection before sending welcome");
            return None;
        }
        Ok(Err(error)) => {
            warn!(%error, "handshake failed");
            return None;
        }
        Err(_) => {
            warn!(?within, "timed out waiting for welcome");
            return None;
        }
    };

    let HostMessage::Welcome {
        protocol,
        ceiling,
        status_interval,
        hotplugged,
    } = first
    else {
        warn!(?first, "host sent a message before welcome");
        return None;
    };

    if protocol != PROTOCOL_VERSION {
        warn!(
            host = protocol,
            agent = PROTOCOL_VERSION,
            "host protocol version mismatch"
        );
        return None;
    }

    Some(Welcome {
        ceiling,
        status_interval,
        hotplugged,
    })
}

async fn pump(
    stream: &mut Stream,
    events: &mpsc::Sender<Event>,
    outgoing: &mut mpsc::Receiver<GuestMessage>,
    clock: Clock,
) -> Outcome {
    let ended = transport::pump(
        stream,
        events,
        |message| Event::Host {
            now: clock.now(),
            message,
        },
        outgoing,
    )
    .await;

    match ended {
        Ended::Closed => {
            info!("host closed the connection");
            Outcome::Retry
        }
        Ended::Recv(error) => {
            warn!(%error, "connection failed");
            Outcome::Retry
        }
        Ended::Send(error) => {
            warn!(%error, "failed to send to the host");
            Outcome::Retry
        }
        Ended::Local => {
            debug!("shutting down");
            Outcome::Closed
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use pazari_proto::{DenyReason, ReclaimId, RequestId};
    use pazari_transport::{Listener, UnixConnector, UnixListener};
    use tempfile::TempDir;
    use tokio::{io::AsyncReadExt as _, net::UnixStream};

    use super::*;
    use crate::engine::testing::{BLOCK, BOOT, CEILING, MIB, bytes, interval, interval_ms};

    type HostStream = pazari_transport::HostStream<UnixStream>;

    fn config(path: &Path) -> SessionConfig {
        SessionConfig {
            handshake_timeout: interval(5),
            reconnect_min: interval_ms(1),
            reconnect_max: interval_ms(5),
            connector: AnyConnector::Unix(UnixConnector::new(path)),
        }
    }

    const fn welcome(protocol: u16) -> HostMessage {
        HostMessage::Welcome {
            protocol,
            ceiling: CEILING,
            status_interval: Duration::from_millis(2_500),
            hotplugged: 0,
        }
    }

    struct Fixture {
        dir: TempDir,
        total_mem: watch::Sender<u64>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                dir: TempDir::new().expect("a temporary directory"),
                total_mem: watch::Sender::new(BOOT),
            }
        }

        fn path(&self) -> PathBuf {
            self.dir.path().join("host.sock")
        }

        fn listen(&self) -> UnixListener {
            UnixListener::bind(self.path()).expect("a bound listener")
        }

        fn spawn(&self) -> (Session, Outbound) {
            Session::spawn(
                config(&self.path()),
                Geometry {
                    block_size: bytes(BLOCK),
                    total_mem: self.total_mem.subscribe(),
                },
                Clock::start(),
            )
        }
    }

    const PATIENCE: Duration = Duration::from_secs(5);

    async fn greet(listener: &UnixListener, answer: Option<HostMessage>) -> HostStream {
        let (connection, _) = timeout(PATIENCE, listener.accept())
            .await
            .expect("the agent dialled in time")
            .expect("a connection");

        let mut stream = HostStream::new(connection);

        let hello = timeout(PATIENCE, stream.recv())
            .await
            .expect("the agent greeted the host in time")
            .expect("a readable connection")
            .expect("a hello");

        assert!(
            matches!(hello, GuestMessage::Hello { protocol, .. } if protocol == PROTOCOL_VERSION),
            "unexpected greeting: {hello:?}"
        );

        if let Some(answer) = answer {
            stream.send(&answer).await.expect("a writable connection");
        }

        stream
    }

    async fn next(session: &mut Session) -> Event {
        timeout(PATIENCE, session.event())
            .await
            .expect("the session answered in time")
            .expect("the session is still running")
    }

    async fn hear(host: &mut HostStream) -> GuestMessage {
        timeout(PATIENCE, host.recv())
            .await
            .expect("the agent spoke in time")
            .expect("a readable connection")
            .expect("a message")
    }

    #[tokio::test]
    async fn a_welcome_becomes_the_connected_event_the_engine_waits_for() {
        let fixture = Fixture::new();
        let listener = fixture.listen();
        let (mut session, outbound) = fixture.spawn();

        let host = greet(&listener, Some(welcome(PROTOCOL_VERSION))).await;

        assert!(
            matches!(
                next(&mut session).await,
                Event::Connected {
                    ceiling: CEILING,
                    status_interval,
                    ..
                } if status_interval == Duration::from_millis(2_500)
            ),
            "the session did not report the host's ceiling and cadence"
        );

        drop(host);
        session.shutdown(outbound).await;
    }

    #[tokio::test]
    async fn the_hello_carries_the_total_memory_of_the_moment() {
        let fixture = Fixture::new();
        let listener = fixture.listen();

        fixture.total_mem.send_replace(BOOT + 512 * MIB);

        let (session, outbound) = fixture.spawn();

        let (connection, _) = timeout(PATIENCE, listener.accept())
            .await
            .expect("the agent dialled in time")
            .expect("a connection");

        let mut host = HostStream::new(connection);

        assert_eq!(
            hear(&mut host).await,
            GuestMessage::Hello {
                protocol: PROTOCOL_VERSION,
                agent_version: env!("CARGO_PKG_VERSION").to_owned(),
                block_size: bytes(BLOCK),
                total_mem: BOOT + 512 * MIB,
            }
        );

        session.shutdown(outbound).await;
    }

    #[tokio::test]
    async fn a_welcome_from_another_protocol_is_dropped_and_the_agent_says_hello_again() {
        let fixture = Fixture::new();
        let listener = fixture.listen();
        let (mut session, outbound) = fixture.spawn();

        let refused = greet(&listener, Some(welcome(PROTOCOL_VERSION + 1))).await;

        let closed = timeout(PATIENCE, refused.into_inner().read(&mut [0_u8; 1]))
            .await
            .expect("the agent let go of the connection in time");

        assert_eq!(
            closed.ok(),
            Some(0),
            "the agent kept a connection it cannot speak on"
        );

        let host = greet(&listener, Some(welcome(PROTOCOL_VERSION))).await;

        assert!(
            matches!(next(&mut session).await, Event::Connected { .. }),
            "the first event was not the second handshake"
        );

        drop(host);
        session.shutdown(outbound).await;
    }

    #[tokio::test]
    async fn a_host_that_closes_mid_stream_disconnects_once_and_comes_back() {
        let fixture = Fixture::new();
        let listener = fixture.listen();
        let (mut session, outbound) = fixture.spawn();

        let host = greet(&listener, Some(welcome(PROTOCOL_VERSION))).await;
        assert!(matches!(next(&mut session).await, Event::Connected { .. }));

        drop(host);

        assert!(
            matches!(next(&mut session).await, Event::Disconnected { .. }),
            "the session did not report the close"
        );

        let host = greet(&listener, Some(welcome(PROTOCOL_VERSION))).await;

        assert!(
            matches!(next(&mut session).await, Event::Connected { .. }),
            "the session reported the same close twice"
        );

        drop(host);
        session.shutdown(outbound).await;
    }

    #[tokio::test]
    async fn messages_sent_while_connected_arrive_in_order() {
        let fixture = Fixture::new();
        let listener = fixture.listen();
        let (mut session, outbound) = fixture.spawn();

        let mut host = greet(&listener, Some(welcome(PROTOCOL_VERSION))).await;
        assert!(matches!(next(&mut session).await, Event::Connected { .. }));

        let sent = vec![
            GuestMessage::GrantRequest {
                id: RequestId(1),
                current: BOOT,
                target: BOOT + 512 * MIB,
            },
            GuestMessage::ReclaimAck {
                id: ReclaimId(4),
                will_release: 256 * MIB,
            },
            GuestMessage::Bye {},
        ];

        for message in &sent {
            outbound.send(message.clone());
        }

        let mut received = Vec::new();
        for _ in 0..sent.len() {
            received.push(hear(&mut host).await);
        }

        assert_eq!(received, sent);

        drop(host);
        session.shutdown(outbound).await;
    }

    #[tokio::test]
    async fn what_the_host_says_reaches_the_engine() {
        let fixture = Fixture::new();
        let listener = fixture.listen();
        let (mut session, outbound) = fixture.spawn();

        let mut host = greet(&listener, Some(welcome(PROTOCOL_VERSION))).await;
        assert!(matches!(next(&mut session).await, Event::Connected { .. }));

        let deny = HostMessage::Deny {
            id: RequestId(1),
            reason: DenyReason::HostPressure,
            retry_after: Some(Duration::from_millis(750)),
        };

        host.send(&deny).await.expect("a writable connection");

        assert!(
            matches!(next(&mut session).await, Event::Host { message, .. } if message == deny),
            "the host's answer did not reach the engine"
        );

        drop(host);
        session.shutdown(outbound).await;
    }

    #[tokio::test]
    async fn a_host_that_says_nothing_is_given_up_on_and_dialled_again() {
        let fixture = Fixture::new();
        let listener = fixture.listen();

        let mut config = config(&fixture.path());
        config.handshake_timeout = interval_ms(50);

        let (session, outbound) = Session::spawn(
            config,
            Geometry {
                block_size: bytes(BLOCK),
                total_mem: fixture.total_mem.subscribe(),
            },
            Clock::start(),
        );

        let silent = greet(&listener, None).await;
        let host = greet(&listener, None).await;

        drop((silent, host));
        session.shutdown(outbound).await;
    }
}
