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
}

impl Listener for UnixListener {
    type Connection = UnixStream;

    fn local(&self) -> PeerId {
        PeerId::Unix {
            path: self.path.clone(),
        }
    }

    async fn accept(&self) -> Result<(Self::Connection, PeerId), TransportError> {
        let (connection, _) = self.inner.accept().await?;
        Ok((connection, self.local()))
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

    fn peer(&self) -> PeerId {
        PeerId::Unix {
            path: self.path.clone(),
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::socket_path;

    #[tokio::test]
    async fn the_listener_identifies_peers_by_its_own_path() {
        let path = socket_path("peer-id");
        let listener = UnixListener::bind(&path).expect("failed to bind the listener");
        let connector = UnixConnector::new(&path);

        assert_eq!(listener.path(), path);
        assert_eq!(connector.path(), path);

        let expected = PeerId::Unix { path: path.clone() };
        assert_eq!(listener.local(), expected);
        assert_eq!(connector.peer(), expected);

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

        let error =
            UnixListener::bind(&path).expect_err("expected a live listener to be respected");
        assert!(
            matches!(&error, TransportError::Io { source } if source.kind() == io::ErrorKind::AddrInUse),
            "unexpected error: {error}"
        );

        drop(connection);
        drop(guest);
        drop(listener);
        let _ = fs::remove_file(&path);
    }
}
