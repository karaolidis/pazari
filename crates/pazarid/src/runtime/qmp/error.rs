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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_socket_failure_names_the_socket_and_the_step() {
        let error = QmpError::Socket {
            op: SocketOp::Connect,
            path: PathBuf::from("/run/pazari/vm0.qmp"),
            source: io::Error::from(io::ErrorKind::ConnectionRefused),
        };

        assert_eq!(
            error.to_string(),
            "failed to connect to the QMP socket /run/pazari/vm0.qmp: connection refused"
        );

        let error = QmpError::Closed {
            path: PathBuf::from("/run/pazari/vm0.qmp"),
        };

        assert_eq!(
            error.to_string(),
            "the QMP connection to /run/pazari/vm0.qmp closed"
        );
    }

    #[test]
    fn a_protocol_break_quotes_what_broke() {
        let error = QmpError::Protocol {
            path: PathBuf::from("/run/pazari/vm0.qmp"),
            reason: "the server sent no greeting".to_owned(),
        };

        assert_eq!(
            error.to_string(),
            "the QMP server at /run/pazari/vm0.qmp broke the protocol: the server sent no greeting"
        );
    }

    #[test]
    fn a_connection_that_died_speaks_with_the_voice_of_what_killed_it() {
        let error = QmpError::Fatal(Arc::new(QmpError::Protocol {
            path: PathBuf::from("/run/pazari/vm0.qmp"),
            reason: "a frame grew past the cap".to_owned(),
        }));

        assert_eq!(
            error.to_string(),
            "the QMP server at /run/pazari/vm0.qmp broke the protocol: a frame grew past the cap"
        );
    }

    #[test]
    fn a_deadline_names_what_ran_out_of_time() {
        let error = QmpError::Timeout {
            during: Operation::Command(QmpCommand::QomGet),
            path: PathBuf::from("/run/pazari/vm0.qmp"),
            deadline: Interval::try_new(Duration::from_secs(5)).expect("five seconds"),
        };

        assert_eq!(
            error.to_string(),
            "qom-get on the QMP socket /run/pazari/vm0.qmp did not finish within 5s"
        );
    }

    #[test]
    fn a_handshake_that_ran_out_of_time_says_so() {
        let error = QmpError::Timeout {
            during: Operation::Handshake,
            path: PathBuf::from("/run/pazari/vm0.qmp"),
            deadline: Interval::try_new(Duration::from_secs(5)).expect("five seconds"),
        };

        assert_eq!(
            error.to_string(),
            "the handshake on the QMP socket /run/pazari/vm0.qmp did not finish within 5s"
        );
    }

    #[test]
    fn a_refused_command_names_itself() {
        let error = QmpError::Refused {
            command: QmpCommand::QomSet,
            reason: "Property 'requested-size' doesn't take value '1'".to_owned(),
        };

        assert_eq!(
            error.to_string(),
            "qemu refused qom-set: Property 'requested-size' doesn't take value '1'"
        );
    }

    #[test]
    fn a_device_that_is_not_there_names_itself() {
        let error = QmpError::NoDevice {
            device: "vm0".to_owned(),
        };

        assert_eq!(
            error.to_string(),
            "the guest has no memory device called vm0"
        );

        let error = QmpError::NotVirtioMem {
            device: "vm0".to_owned(),
            kind: "pc-dimm".to_owned(),
        };

        assert_eq!(
            error.to_string(),
            "the memory device vm0 is a pc-dimm, not a virtio-mem"
        );
    }
}
