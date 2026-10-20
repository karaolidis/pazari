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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_names_the_file_it_could_not_use() {
        let error = RuntimeError::Read {
            path: PathBuf::from("/sys/fs/cgroup/workload/memory.current"),
            source: io::Error::from(io::ErrorKind::NotFound),
        };

        assert_eq!(
            error.to_string(),
            "failed to read /sys/fs/cgroup/workload/memory.current: entity not found"
        );
    }

    #[test]
    fn a_rejected_write_carries_what_was_written() {
        let error = RuntimeError::Write {
            path: PathBuf::from("/sys/fs/cgroup/workload/memory.reclaim"),
            value: "67108864 swappiness=0".to_owned(),
            source: io::Error::from(io::ErrorKind::InvalidInput),
        };

        assert_eq!(
            error.to_string(),
            "failed to write \"67108864 swappiness=0\" to /sys/fs/cgroup/workload/memory.reclaim: invalid input parameter"
        );
    }

    #[test]
    fn a_malformed_file_quotes_what_it_held() {
        let error = RuntimeError::from(KernelFileError::Malformed {
            path: PathBuf::from("/sys/devices/system/memory/block_size_bytes"),
            value: "wat".to_owned(),
            expected: "a lowercase hexadecimal byte count",
        });

        assert_eq!(
            error.to_string(),
            "/sys/devices/system/memory/block_size_bytes holds \"wat\", which is not a lowercase hexadecimal byte count"
        );
    }

    #[test]
    fn a_netlink_failure_names_the_step_it_failed_at() {
        let error = RuntimeError::Netlink {
            step: NetlinkStep::Wait,
            source: io::Error::from(io::ErrorKind::PermissionDenied),
        };

        assert_eq!(
            error.to_string(),
            "failed to wait on the kobject uevent netlink socket: permission denied"
        );
    }

    #[test]
    fn a_second_reclaim_names_the_one_already_running() {
        let error = RuntimeError::ReclaimInFlight {
            bytes: 128,
            running: 256,
        };

        assert_eq!(
            error.to_string(),
            "asked to reclaim 128 bytes while a reclaim of 256 bytes is still running"
        );
    }
}
