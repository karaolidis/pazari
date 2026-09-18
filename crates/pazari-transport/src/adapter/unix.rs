use std::{
    fs, io,
    os::unix::fs::FileTypeExt,
    path::{Path, PathBuf},
};

use tokio::net::UnixStream;

use crate::{Connector, Listener, PeerId, TransportError};

#[derive(Debug)]
pub struct UnixListener {
    inner: tokio::net::UnixListener,
    path: PathBuf,
}

impl UnixListener {
    pub fn bind(path: impl Into<PathBuf>) -> Result<Self, TransportError> {
        let path = path.into();
        clear_stale(&path)?;

        Ok(Self {
            inner: tokio::net::UnixListener::bind(&path)?,
            path,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn peer(&self) -> PeerId {
        PeerId::Unix {
            path: self.path.clone(),
        }
    }
}

impl Listener for UnixListener {
    type Connection = UnixStream;

    async fn accept(&self) -> Result<(Self::Connection, PeerId), TransportError> {
        let (connection, _) = self.inner.accept().await?;
        Ok((connection, self.peer()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UnixConnector {
    path: PathBuf,
}

impl UnixConnector {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Connector for UnixConnector {
    type Connection = UnixStream;

    async fn connect(&self) -> Result<Self::Connection, TransportError> {
        Ok(UnixStream::connect(&self.path).await?)
    }
}

fn clear_stale(path: &Path) -> Result<(), TransportError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };

    if !metadata.file_type().is_socket() {
        return Ok(());
    }

    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("{} is bound by a live listener", path.display()),
        )
        .into()),
        Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
            fs::remove_file(path)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}
