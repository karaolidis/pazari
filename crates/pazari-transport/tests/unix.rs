use std::{
    fs, io,
    path::PathBuf,
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

use pazari_proto::{
    Codec, DecodeError, DenyReason, GrantReason, GuestMessage, HostMessage, JsonLinesCodec,
    MAX_LINE_BYTES, PROTOCOL_VERSION, Urgency,
};
use pazari_transport::{
    AnyConnection, AnyConnector, AnyListener, Connector, Listener, MessageStream, PeerId,
    TransportError, UnixConnector, UnixListener,
};
use tokio::{io::AsyncWriteExt, net::UnixStream};

type GuestStream<T> = MessageStream<T, JsonLinesCodec, HostMessage, GuestMessage>;
type HostStream<T> = MessageStream<T, JsonLinesCodec, GuestMessage, HostMessage>;

static SEQUENCE: AtomicU32 = AtomicU32::new(0);

fn socket_path(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "pazari-transport-{}-{name}-{}.sock",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    path
}

fn guest_messages() -> Vec<GuestMessage> {
    vec![
        GuestMessage::Hello {
            protocol: PROTOCOL_VERSION,
            agent_version: "0.1.0".to_owned(),
            boot_mem: 2_147_483_648,
            block_size: 2_097_152,
            plugged: 0,
        },
        GuestMessage::GrantRequest {
            id: 1,
            current: 2_147_483_648,
            target: 4_294_967_296,
            reason: GrantReason::Throttle,
            urgency: Urgency::High,
        },
        GuestMessage::Status {
            plugged: 2_147_483_648,
            available: 536_870_912,
            high_events: 17,
            max_events: 0,
            psi_some10: 12.5,
            psi_full10: 0.25,
            swap_used: 0,
            workload_usage: 1_610_612_736,
        },
        GuestMessage::Bye {},
    ]
}

fn host_messages() -> Vec<HostMessage> {
    vec![
        HostMessage::Welcome {
            protocol: PROTOCOL_VERSION,
            ceiling: 8_589_934_592,
            status_interval_ms: Duration::from_millis(2_500),
        },
        HostMessage::Ceiling {
            ceiling: 6_442_450_944,
        },
        HostMessage::Deny {
            id: 1,
            reason: DenyReason::RateLimited,
            retry_after_ms: Some(Duration::from_millis(750)),
        },
        HostMessage::ReclaimRequest {
            id: 9,
            desired: 1_073_741_824,
            deadline_ms: Duration::from_secs(30),
        },
    ]
}

fn framed<M: serde::Serialize>(messages: &[M]) -> Vec<u8> {
    let mut out = Vec::new();
    for message in messages {
        JsonLinesCodec::encode(message, &mut out).expect("failed to encode a message");
    }
    out
}

#[tokio::test]
async fn messages_travel_in_both_directions() {
    let path = socket_path("both-ways");
    let listener = UnixListener::bind(&path).unwrap();
    let connector = UnixConnector::new(&path);

    let expected_peer = PeerId::Unix { path: path.clone() };

    let host = tokio::spawn(async move {
        let (connection, peer) = listener.accept().await.unwrap();
        assert_eq!(peer, expected_peer);

        let mut stream = HostStream::new(connection);

        let mut received = Vec::new();
        for _ in 0..guest_messages().len() {
            received.push(stream.recv().await.unwrap().unwrap());
        }
        assert_eq!(received, guest_messages());

        for message in &host_messages() {
            stream.send(message).await.unwrap();
        }
    });

    let mut guest = GuestStream::new(connector.connect().await.unwrap());
    for message in &guest_messages() {
        guest.send(message).await.unwrap();
    }

    let mut received = Vec::new();
    for _ in 0..host_messages().len() {
        received.push(guest.recv().await.unwrap().unwrap());
    }
    assert_eq!(received, host_messages());

    host.await.unwrap();
    let _ = fs::remove_file(&path);
}

#[tokio::test]
async fn a_message_split_into_small_writes_arrives_whole() {
    let path = socket_path("small-writes");
    let listener = UnixListener::bind(&path).unwrap();
    let connector = UnixConnector::new(&path);

    let message = guest_messages().swap_remove(2);
    let wire = framed(std::slice::from_ref(&message));

    let guest = tokio::spawn(async move {
        let mut connection = connector.connect().await.unwrap();
        for chunk in wire.chunks(3) {
            connection.write_all(chunk).await.unwrap();
            connection.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });

    let (connection, _) = listener.accept().await.unwrap();
    let mut stream = HostStream::new(connection);

    assert_eq!(stream.recv().await.unwrap(), Some(message));

    guest.await.unwrap();
    assert_eq!(stream.recv().await.unwrap(), None);

    let _ = fs::remove_file(&path);
}

#[tokio::test]
async fn messages_written_in_one_burst_arrive_in_order() {
    let path = socket_path("burst");
    let listener = UnixListener::bind(&path).unwrap();
    let connector = UnixConnector::new(&path);

    let messages = guest_messages();
    let wire = framed(&messages);

    let guest = tokio::spawn(async move {
        let mut connection = connector.connect().await.unwrap();
        connection.write_all(&wire).await.unwrap();
        connection.flush().await.unwrap();
    });

    let (connection, _) = listener.accept().await.unwrap();
    let mut stream = HostStream::new(connection);

    let mut received = Vec::new();
    for _ in 0..messages.len() {
        received.push(stream.recv().await.unwrap().unwrap());
    }

    assert_eq!(received, messages);

    guest.await.unwrap();
    let _ = fs::remove_file(&path);
}

#[tokio::test]
async fn a_clean_close_ends_the_stream() {
    let path = socket_path("clean-close");
    let listener = UnixListener::bind(&path).unwrap();
    let connector = UnixConnector::new(&path);

    let guest = tokio::spawn(async move {
        let mut stream = GuestStream::new(connector.connect().await.unwrap());
        stream.send(&GuestMessage::Bye {}).await.unwrap();
    });

    let (connection, _) = listener.accept().await.unwrap();
    let mut stream = HostStream::new(connection);

    assert_eq!(stream.recv().await.unwrap(), Some(GuestMessage::Bye {}));

    guest.await.unwrap();
    assert_eq!(stream.recv().await.unwrap(), None);

    let _ = fs::remove_file(&path);
}

#[tokio::test]
async fn a_close_mid_message_is_truncated() {
    let path = socket_path("truncated");
    let listener = UnixListener::bind(&path).unwrap();
    let connector = UnixConnector::new(&path);

    let wire = framed(std::slice::from_ref(&GuestMessage::Bye {}));
    let partial = wire.len() - 2;
    let prefix = wire[..partial].to_vec();

    let guest = tokio::spawn(async move {
        let mut connection = connector.connect().await.unwrap();
        connection.write_all(&prefix).await.unwrap();
        connection.flush().await.unwrap();
    });

    let (connection, _) = listener.accept().await.unwrap();
    let mut stream = HostStream::new(connection);

    let error = stream
        .recv()
        .await
        .expect_err("expected a truncated stream");
    assert!(
        matches!(error, TransportError::Truncated { pending } if pending == partial),
        "unexpected error: {error}"
    );

    guest.await.unwrap();
    let _ = fs::remove_file(&path);
}

async fn violation(name: &str, raw: Vec<u8>) -> (TransportError, TransportError) {
    let path = socket_path(name);
    let listener = UnixListener::bind(&path).expect("failed to bind the listener");
    let connector = UnixConnector::new(&path);

    let guest = tokio::spawn(async move {
        let mut connection = connector.connect().await.expect("failed to connect");
        let _ = connection.write_all(&raw).await;
        let _ = connection.flush().await;
    });

    let (connection, _) = listener.accept().await.expect("failed to accept");
    let mut stream = HostStream::new(connection);

    let first = stream
        .recv()
        .await
        .expect_err("expected a protocol violation");
    let second = stream
        .recv()
        .await
        .expect_err("expected the decoder to stay poisoned");

    guest.abort();
    let _ = fs::remove_file(&path);

    (first, second)
}

#[tokio::test]
async fn an_over_long_line_is_fatal_and_keeps_failing() {
    let (first, second) = violation("over-long", vec![b'x'; MAX_LINE_BYTES + 1]).await;

    assert!(
        matches!(
            first,
            TransportError::Decode {
                source: DecodeError::LineTooLong { .. }
            }
        ),
        "unexpected error: {first}"
    );
    assert!(
        matches!(
            second,
            TransportError::Decode {
                source: DecodeError::Poisoned
            }
        ),
        "unexpected error: {second}"
    );
}

#[tokio::test]
async fn a_malformed_line_is_fatal_and_keeps_failing() {
    let (first, second) = violation("malformed", b"{\"type\":\"bye\"\n".to_vec()).await;

    assert!(
        matches!(
            first,
            TransportError::Decode {
                source: DecodeError::Malformed { .. }
            }
        ),
        "unexpected error: {first}"
    );
    assert!(
        matches!(
            second,
            TransportError::Decode {
                source: DecodeError::Poisoned
            }
        ),
        "unexpected error: {second}"
    );
}

#[tokio::test]
async fn the_listener_identifies_peers_by_its_own_path() {
    let path = socket_path("peer-id");
    let listener = UnixListener::bind(&path).expect("failed to bind the listener");
    let connector = UnixConnector::new(&path);

    assert_eq!(listener.path(), path);
    assert_eq!(connector.path(), path);

    let expected = PeerId::Unix { path: path.clone() };
    assert_eq!(expected.to_string(), format!("unix:{}", path.display()));

    for _ in 0..3u8 {
        let guest = connector.connect().await.unwrap();
        let (connection, peer) = listener.accept().await.unwrap();

        assert_eq!(peer, expected);

        drop(connection);
        drop(guest);
    }

    let _ = fs::remove_file(&path);
}

#[tokio::test]
async fn a_stale_socket_file_is_rebound() {
    let path = socket_path("stale");

    drop(UnixListener::bind(&path).unwrap());
    assert!(fs::symlink_metadata(&path).is_ok());

    let listener = UnixListener::bind(&path).expect("failed to bind the listener");
    let connector = UnixConnector::new(&path);

    let guest = connector.connect().await.unwrap();
    let (connection, peer) = listener.accept().await.unwrap();
    assert_eq!(peer, PeerId::Unix { path: path.clone() });

    let error = UnixListener::bind(&path).expect_err("expected a live listener to be respected");
    assert!(
        matches!(&error, TransportError::Io { source } if source.kind() == io::ErrorKind::AddrInUse),
        "unexpected error: {error}"
    );

    drop(connection);
    drop(guest);
    drop(listener);
    let _ = fs::remove_file(&path);
}

#[tokio::test]
async fn the_runtime_wrappers_carry_messages() {
    let path = socket_path("any");
    let listener = AnyListener::Unix(UnixListener::bind(&path).unwrap());
    let connector = AnyConnector::Unix(UnixConnector::new(&path));

    let host = tokio::spawn(async move {
        let (connection, peer) = listener.accept().await.unwrap();
        assert!(matches!(peer, PeerId::Unix { .. }));

        let mut stream: HostStream<AnyConnection> = MessageStream::new(connection);
        assert_eq!(stream.recv().await.unwrap(), Some(GuestMessage::Bye {}));

        stream
            .send(&HostMessage::Ceiling { ceiling: 42 })
            .await
            .unwrap();
    });

    let mut guest: GuestStream<AnyConnection> =
        MessageStream::new(connector.connect().await.unwrap());
    guest.send(&GuestMessage::Bye {}).await.unwrap();

    assert_eq!(
        guest.recv().await.unwrap(),
        Some(HostMessage::Ceiling { ceiling: 42 })
    );

    host.await.unwrap();
    let _ = fs::remove_file(&path);
}

#[tokio::test]
async fn a_typed_stream_releases_its_connection() {
    let path = socket_path("into-inner");
    let listener = UnixListener::bind(&path).expect("failed to bind the listener");
    let connector = UnixConnector::new(&path);

    let guest = connector.connect().await.unwrap();
    let (connection, _) = listener.accept().await.unwrap();

    let stream: HostStream<UnixStream> = MessageStream::new(connection);
    let connection = stream.into_inner();
    assert!(connection.peer_addr().is_ok());

    drop(guest);
    let _ = fs::remove_file(&path);
}
