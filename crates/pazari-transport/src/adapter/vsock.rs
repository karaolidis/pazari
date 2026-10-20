pub use tokio_vsock::{VMADDR_CID_ANY, VMADDR_CID_HOST, VMADDR_CID_LOCAL};
use tokio_vsock::{VsockAddr, VsockStream};

use crate::{Connector, Listener, PeerId, TransportError};

/// The port pazarid listens on unless told otherwise.
pub const DEFAULT_VSOCK_PORT: u32 = 4650;

#[derive(Debug)]
pub struct VsockListener {
    inner: tokio_vsock::VsockListener,
    address: VsockAddr,
}

impl VsockListener {
    pub fn bind(cid: u32, port: u32) -> Result<Self, TransportError> {
        let inner = tokio_vsock::VsockListener::bind(VsockAddr::new(cid, port))?;
        let address = inner.local_addr()?;

        Ok(Self { inner, address })
    }

    pub fn bind_any(port: u32) -> Result<Self, TransportError> {
        Self::bind(VMADDR_CID_ANY, port)
    }

    #[must_use]
    pub fn port(&self) -> u32 {
        self.address.port()
    }
}

impl Listener for VsockListener {
    type Connection = VsockStream;

    fn local(&self) -> PeerId {
        PeerId::Vsock {
            cid: self.address.cid(),
        }
    }

    async fn accept(&self) -> Result<(Self::Connection, PeerId), TransportError> {
        let (connection, address) = self.inner.accept().await?;
        Ok((connection, PeerId::Vsock { cid: address.cid() }))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VsockConnector {
    cid: u32,
    port: u32,
}

impl VsockConnector {
    #[must_use]
    pub const fn new(cid: u32, port: u32) -> Self {
        Self { cid, port }
    }

    #[must_use]
    pub const fn cid(&self) -> u32 {
        self.cid
    }

    #[must_use]
    pub const fn port(&self) -> u32 {
        self.port
    }
}

impl Default for VsockConnector {
    fn default() -> Self {
        Self::new(VMADDR_CID_HOST, DEFAULT_VSOCK_PORT)
    }
}

impl Connector for VsockConnector {
    type Connection = VsockStream;

    fn peer(&self) -> PeerId {
        PeerId::Vsock { cid: self.cid }
    }

    async fn connect(&self) -> Result<Self::Connection, TransportError> {
        Ok(VsockStream::connect(VsockAddr::new(self.cid, self.port)).await?)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pazari_proto::{
        GuestMessage, HostMessage,
        testing::{guest_messages, host_messages},
    };

    use super::*;
    use crate::testing::{GuestStream, HostStream};

    const ACCEPT_TIMEOUT: Duration = Duration::from_secs(5);

    fn loopback_port() -> u32 {
        0x7061_0000 | (std::process::id() & 0xFFFF)
    }

    fn hello() -> GuestMessage {
        guest_messages().swap_remove(0)
    }

    fn welcome() -> HostMessage {
        host_messages().swap_remove(0)
    }

    #[test]
    fn the_connector_defaults_to_the_host_cid_and_the_protocol_port() {
        let connector = VsockConnector::default();

        assert_eq!(connector.cid(), VMADDR_CID_HOST);
        assert_eq!(connector.port(), DEFAULT_VSOCK_PORT);
        assert_eq!(
            connector.peer(),
            PeerId::Vsock {
                cid: VMADDR_CID_HOST
            }
        );
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

        assert_eq!(listener.port(), port);
        assert_eq!(
            listener.local(),
            PeerId::Vsock {
                cid: VMADDR_CID_ANY
            }
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

        let (accepted, peer) = tokio::time::timeout(ACCEPT_TIMEOUT, listener.accept())
            .await
            .expect("timed out waiting for the loopback connection")
            .expect("failed to accept the loopback connection");

        assert_eq!(
            peer,
            PeerId::Vsock {
                cid: VMADDR_CID_LOCAL
            }
        );

        let mut guest = GuestStream::new(connection);
        let mut host = HostStream::new(accepted);

        guest.send(&hello()).await.unwrap();
        assert_eq!(host.recv().await.unwrap(), Some(hello()));

        host.send(&welcome()).await.unwrap();
        assert_eq!(guest.recv().await.unwrap(), Some(welcome()));

        drop(guest);
        assert_eq!(host.recv().await.unwrap(), None);
    }
}
