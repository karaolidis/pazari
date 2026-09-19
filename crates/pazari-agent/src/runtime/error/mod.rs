use std::{io, path::PathBuf};

use thiserror::Error;
use tokio::task::JoinError;

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

    #[error("{} holds {value:?}, which is not {expected}", .path.display())]
    Malformed {
        path: PathBuf,
        value: String,
        expected: &'static str,
    },

    #[error("{} carries no {key} line", .path.display())]
    Absent { path: PathBuf, key: &'static str },

    #[error("failed to watch {} for throttle events: {source}", .path.display())]
    Watch { path: PathBuf, source: io::Error },

    #[error("failed to {what} the kobject uevent netlink socket: {source}")]
    Netlink {
        what: &'static str,
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
        let error = RuntimeError::Malformed {
            path: PathBuf::from("/sys/devices/system/memory/block_size_bytes"),
            value: "wat".to_owned(),
            expected: "a lowercase hexadecimal byte count",
        };

        assert_eq!(
            error.to_string(),
            "/sys/devices/system/memory/block_size_bytes holds \"wat\", which is not a lowercase hexadecimal byte count"
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
