use std::{fmt, io, path::PathBuf, sync::Arc};

use pazari_types::Interval;
use thiserror::Error;

use crate::runtime::qmp::wire::QmpCommand;

/// What the daemon was doing with a QMP socket when it failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketOp {
    Connect,
    Read,
    Write,
}

impl fmt::Display for SocketOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Connect => "connect to",
            Self::Read => "read from",
            Self::Write => "write to",
        })
    }
}

/// What a QMP deadline was spent waiting on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Handshake,
    Command(QmpCommand),
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Handshake => f.write_str("the handshake"),
            Self::Command(command) => command.fmt(f),
        }
    }
}

#[derive(Debug, Error)]
pub enum QmpError {
    #[error("failed to {op} the QMP socket {}: {source}", .path.display())]
    Socket {
        op: SocketOp,
        path: PathBuf,
        source: io::Error,
    },

    #[error("the QMP server at {} broke the protocol: {reason}", .path.display())]
    Protocol { path: PathBuf, reason: String },

    #[error("the QMP connection to {} closed", .path.display())]
    Closed { path: PathBuf },

    #[error("{0}")]
    Fatal(Arc<Self>),

    #[error("qemu refused {command}: {reason}")]
    Refused { command: QmpCommand, reason: String },

    #[error("{during} on the QMP socket {} did not finish within {deadline}", .path.display())]
    Timeout {
        during: Operation,
        path: PathBuf,
        deadline: Interval,
    },

    #[error("the guest has no memory device called {device}")]
    NoDevice { device: String },

    #[error("the memory device {device} is a {kind}, not a virtio-mem")]
    NotVirtioMem { device: String, kind: String },
}
