use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::UnixStream,
};
use tokio_vsock::VsockStream;

use crate::{
    Connector, Listener, PeerId, TransportError, UnixConnector, UnixListener, VsockConnector,
    VsockListener,
};

#[derive(Debug)]
pub enum AnyListener {
    Unix(UnixListener),
    Vsock(VsockListener),
}

impl Listener for AnyListener {
    type Connection = AnyConnection;

    async fn accept(&self) -> Result<(Self::Connection, PeerId), TransportError> {
        match self {
            Self::Unix(listener) => {
                let (connection, peer) = listener.accept().await?;
                Ok((AnyConnection::Unix(connection), peer))
            }
            Self::Vsock(listener) => {
                let (connection, peer) = listener.accept().await?;
                Ok((AnyConnection::Vsock(connection), peer))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AnyConnector {
    Unix(UnixConnector),
    Vsock(VsockConnector),
}

impl Connector for AnyConnector {
    type Connection = AnyConnection;

    async fn connect(&self) -> Result<Self::Connection, TransportError> {
        match self {
            Self::Unix(connector) => Ok(AnyConnection::Unix(connector.connect().await?)),
            Self::Vsock(connector) => Ok(AnyConnection::Vsock(connector.connect().await?)),
        }
    }
}

#[derive(Debug)]
pub enum AnyConnection {
    Unix(UnixStream),
    Vsock(VsockStream),
}

impl AsyncRead for AnyConnection {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Unix(connection) => Pin::new(connection).poll_read(context, buf),
            Self::Vsock(connection) => Pin::new(connection).poll_read(context, buf),
        }
    }
}

impl AsyncWrite for AnyConnection {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Unix(connection) => Pin::new(connection).poll_write(context, buf),
            Self::Vsock(connection) => Pin::new(connection).poll_write(context, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Unix(connection) => Pin::new(connection).poll_flush(context),
            Self::Vsock(connection) => Pin::new(connection).poll_flush(context),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Unix(connection) => Pin::new(connection).poll_shutdown(context),
            Self::Vsock(connection) => Pin::new(connection).poll_shutdown(context),
        }
    }
}
