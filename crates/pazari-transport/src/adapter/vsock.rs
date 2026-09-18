use pazari_proto::DEFAULT_VSOCK_PORT;
pub use tokio_vsock::{VMADDR_CID_ANY, VMADDR_CID_HOST, VMADDR_CID_LOCAL};
use tokio_vsock::{VsockAddr, VsockStream};

use crate::{Connector, Listener, PeerId, TransportError};

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
