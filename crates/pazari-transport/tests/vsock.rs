use std::time::Duration;

use pazari_proto::{GuestMessage, HostMessage, JsonLinesCodec, PROTOCOL_VERSION};
use pazari_transport::{
    Connector, Listener, MessageStream, PeerId, VsockConnector, VsockListener,
    adapter::vsock::VMADDR_CID_LOCAL,
};

type GuestStream<T> = MessageStream<T, JsonLinesCodec, HostMessage, GuestMessage>;
type HostStream<T> = MessageStream<T, JsonLinesCodec, GuestMessage, HostMessage>;

const ACCEPT_TIMEOUT: Duration = Duration::from_secs(5);

fn loopback_port() -> u32 {
    0x7061_0000 | (std::process::id() & 0xFFFF)
}

fn hello() -> GuestMessage {
    GuestMessage::Hello {
        protocol: PROTOCOL_VERSION,
        agent_version: "0.1.0".to_owned(),
        boot_mem: 1_073_741_824,
        block_size: 2_097_152,
        plugged: 0,
    }
}

const fn welcome() -> HostMessage {
    HostMessage::Welcome {
        protocol: PROTOCOL_VERSION,
        ceiling: 4_294_967_296,
        status_interval_ms: Duration::from_secs(1),
    }
}

#[tokio::test]
async fn a_loopback_round_trip_carries_messages() {
    let port = loopback_port();

    let listener = match VsockListener::bind_any(port) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("skipping: cannot bind an AF_VSOCK listener: {error}");
            return;
        }
    };

    assert_eq!(
        listener.port().expect("failed to read the bound port"),
        port
    );

    let connector = VsockConnector::new(VMADDR_CID_LOCAL, port);
    assert_eq!(connector.cid(), VMADDR_CID_LOCAL);
    assert_eq!(connector.port(), port);

    let connection = match connector.connect().await {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("skipping: AF_VSOCK loopback is unavailable: {error}");
            return;
        }
    };

    let accepted = tokio::time::timeout(ACCEPT_TIMEOUT, listener.accept())
        .await
        .expect("timed out waiting for the loopback connection")
        .expect("failed to accept the loopback connection");

    let (accepted, peer) = accepted;
    assert_eq!(
        peer,
        PeerId::Vsock {
            cid: VMADDR_CID_LOCAL
        }
    );
    assert_eq!(peer.to_string(), format!("vsock:{VMADDR_CID_LOCAL}"));

    let mut guest = GuestStream::new(connection);
    let mut host = HostStream::new(accepted);

    guest.send(&hello()).await.unwrap();
    assert_eq!(host.recv().await.unwrap(), Some(hello()));

    host.send(&welcome()).await.unwrap();
    assert_eq!(guest.recv().await.unwrap(), Some(welcome()));

    drop(guest);
    assert_eq!(host.recv().await.unwrap(), None);
}

#[test]
fn the_connector_defaults_to_the_host_cid_and_the_protocol_port() {
    let connector = VsockConnector::default();

    assert_eq!(
        connector.cid(),
        pazari_transport::adapter::vsock::VMADDR_CID_HOST
    );
    assert_eq!(connector.port(), pazari_proto::DEFAULT_VSOCK_PORT);
}
