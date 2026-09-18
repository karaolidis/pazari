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

    fn local(&self) -> PeerId {
        match self {
            Self::Unix(listener) => listener.local(),
            Self::Vsock(listener) => listener.local(),
        }
    }

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

    fn peer(&self) -> PeerId {
        match self {
            Self::Unix(connector) => connector.peer(),
            Self::Vsock(connector) => connector.peer(),
        }
    }

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

#[cfg(test)]
mod tests {
    use std::fs;

    use pazari_proto::{GuestMessage, HostMessage};

    use super::*;
    use crate::{
        MessageStream,
        testing::{GuestStream, HostStream, socket_path},
    };

    #[tokio::test]
    async fn the_runtime_wrappers_carry_messages() {
        let path = socket_path("any");
        let listener = AnyListener::Unix(UnixListener::bind(&path).unwrap());
        let connector = AnyConnector::Unix(UnixConnector::new(&path));

        let expected = PeerId::Unix { path: path.clone() };
        assert_eq!(listener.local(), expected);
        assert_eq!(connector.peer(), expected);

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
}
