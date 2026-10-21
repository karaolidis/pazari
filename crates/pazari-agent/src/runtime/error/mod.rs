use std::{fmt, io, path::PathBuf};

use pazari_types::KernelFileError;
use thiserror::Error;
use tokio::task::JoinError;

/// What the agent was doing with the kobject uevent socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetlinkStep {
    Open,
    Bind,
    Watch,
    Wait,
    Read,
}

impl fmt::Display for NetlinkStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Open => "open",
            Self::Bind => "bind",
            Self::Watch => "watch",
            Self::Wait => "wait on",
            Self::Read => "read from",
        })
    }
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("failed to read {}: {source}", .path.display())]
    Read { path: PathBuf, source: io::Error },

    #[error("failed to write {value:?} to {}: {source}", .path.display())]
    Write {
        path: PathBuf,
        value: String,
        source: io::Error,
    },

    #[error("failed to watch {} for throttle events: {source}", .path.display())]
    Watch { path: PathBuf, source: io::Error },

    #[error("failed to {step} the kobject uevent netlink socket: {source}")]
    Netlink {
        step: NetlinkStep,
        source: io::Error,
    },

    #[error("failed to reach the system bus: {source}")]
    Bus { source: Box<zbus::Error> },

    #[error("systemd refused to set {property} on {unit}: {source}")]
    Property {
        unit: String,
        property: &'static str,
        source: Box<zbus::Error>,
    },

    #[error("asked to reclaim {bytes} bytes while a reclaim of {running} bytes is still running")]
    ReclaimInFlight { bytes: u64, running: u64 },

    #[error("the reclaim task died: {source}")]
    ReclaimLost { source: JoinError },

    #[error(transparent)]
    Kernel(#[from] KernelFileError),
}
