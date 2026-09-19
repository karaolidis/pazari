use std::{io, path::PathBuf, sync::Arc};

use pazari_types::Interval;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum QmpError {
    #[error("failed to {what} the QMP socket {}: {source}", .path.display())]
    Socket {
        what: &'static str,
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
    Refused {
        command: &'static str,
        reason: String,
    },

    #[error("{what} on the QMP socket {} did not finish within {deadline}", .path.display())]
    Timeout {
        what: &'static str,
        path: PathBuf,
        deadline: Interval,
    },

    #[error("the guest has no memory device called {device}")]
    NoDevice { device: String },

    #[error("the memory device {device} is a {kind}, not a virtio-mem")]
    NotVirtioMem { device: String, kind: String },

    #[error("asked the device for {bytes}, under the guest's boot memory of {boot}")]
    BelowBoot { bytes: u64, boot: u64 },
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_socket_failure_names_the_socket_and_the_step() {
        let error = QmpError::Socket {
            what: "connect to",
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
            what: "qom-get",
            path: PathBuf::from("/run/pazari/vm0.qmp"),
            deadline: Interval::try_new(Duration::from_secs(5)).expect("five seconds"),
        };

        assert_eq!(
            error.to_string(),
            "qom-get on the QMP socket /run/pazari/vm0.qmp did not finish within 5s"
        );
    }

    #[test]
    fn a_refused_command_names_itself() {
        let error = QmpError::Refused {
            command: "qom-set",
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

    #[test]
    fn an_ask_under_the_boot_memory_names_both_sizes() {
        let error = QmpError::BelowBoot {
            bytes: 1 << 30,
            boot: 1 << 31,
        };

        assert_eq!(
            error.to_string(),
            "asked the device for 1073741824, under the guest's boot memory of 2147483648"
        );
    }
}
