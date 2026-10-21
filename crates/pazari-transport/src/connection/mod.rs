use std::{fmt, future::Future, path::PathBuf};

use tokio::io::{AsyncRead, AsyncWrite};

use crate::TransportError;

pub trait Connection: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> Connection for T {}

pub trait Listener {
    type Connection: Connection;

    fn local(&self) -> PeerId;

    fn accept(
        &self,
    ) -> impl Future<Output = Result<(Self::Connection, PeerId), TransportError>> + Send;
}

pub trait Connector {
    type Connection: Connection;

    fn peer(&self) -> PeerId;

    fn connect(&self) -> impl Future<Output = Result<Self::Connection, TransportError>> + Send;
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PeerId {
    Vsock { cid: u32 },
    Unix { path: PathBuf },
}

impl fmt::Display for PeerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Vsock { cid } => write!(formatter, "vsock:{cid}"),
            Self::Unix { path } => write!(formatter, "unix:{}", path.display()),
        }
    }
}
