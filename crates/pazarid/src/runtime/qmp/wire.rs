use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::runtime::qmp::error::QmpError;

pub const CAPABILITIES: &str = "qmp_capabilities";
pub const QUERY_DEVICES: &str = "query-memory-devices";
pub const QUERY_SUMMARY: &str = "query-memory-size-summary";
pub const QOM_GET: &str = "qom-get";
pub const QOM_SET: &str = "qom-set";

pub const MEMORY_DEVICE_SIZE_CHANGE: &str = "MEMORY_DEVICE_SIZE_CHANGE";

#[derive(Debug, Serialize)]
pub struct Command<'a> {
    pub execute: &'a str,
    pub id: u64,

    #[serde(skip_serializing_if = "Value::is_null")]
    pub arguments: Value,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Incoming {
    Greeting(Greeting),
    Reply(Reply),
    Event(Event),
    Fault(Fault),
}

#[derive(Debug, Deserialize)]
pub struct Greeting {
    #[serde(rename = "QMP")]
    pub qmp: Handshake,
}

#[derive(Debug, Deserialize)]
pub struct Handshake {
    pub capabilities: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Reply {
    pub id: u64,

    #[serde(flatten)]
    pub outcome: Outcome,
}

#[derive(Debug, Deserialize)]
pub enum Outcome {
    #[serde(rename = "return")]
    Return(Value),

    #[serde(rename = "error")]
    Error(Failure),
}

impl Outcome {
    pub fn into_result(self, command: &'static str) -> Result<Value, QmpError> {
        match self {
            Self::Return(value) => Ok(value),
            Self::Error(failure) => Err(QmpError::Refused {
                command,
                reason: failure.desc,
            }),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Failure {
    pub class: String,
    pub desc: String,
}

#[derive(Debug, Deserialize)]
pub struct Fault {
    pub error: Failure,
}

#[derive(Debug, Deserialize)]
pub struct Event {
    pub event: String,

    #[serde(default)]
    pub data: Value,
}

/// `MEMORY_DEVICE_SIZE_CHANGE`'s payload.
#[derive(Debug, Deserialize)]
pub struct SizeChange {
    pub id: Option<String>,
    pub size: u64,

    #[serde(rename = "qom-path")]
    pub qom_path: Option<String>,
}

/// One entry of `query-memory-devices`.
#[derive(Debug, Deserialize)]
pub struct MemoryDevice {
    #[serde(rename = "type")]
    pub kind: String,

    #[serde(default)]
    pub data: DeviceData,
}

#[derive(Debug, Default, Deserialize)]
pub struct DeviceData {
    pub id: Option<String>,
}

/// What `query-memory-size-summary` reports.
#[derive(Debug, Deserialize)]
pub struct MemorySummary {
    #[serde(rename = "base-memory")]
    pub base_memory: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn incoming(line: &str) -> Incoming {
        serde_json::from_str(line).expect("a QMP frame")
    }

    #[test]
    fn a_greeting_is_told_apart_from_a_reply_and_an_event() {
        assert!(matches!(
            incoming(
                r#"{"QMP":{"version":{"qemu":{"major":10,"minor":1,"micro":0}},"capabilities":["oob"]}}"#
            ),
            Incoming::Greeting(greeting) if greeting.qmp.capabilities == ["oob"]
        ));

        assert!(matches!(
            incoming(r#"{"return":{"size":1073741824},"id":7}"#),
            Incoming::Reply(Reply {
                id: 7,
                outcome: Outcome::Return(_)
            })
        ));

        assert!(matches!(
            incoming(r#"{"error":{"class":"GenericError","desc":"no"},"id":3}"#),
            Incoming::Reply(Reply {
                id: 3,
                outcome: Outcome::Error(failure)
            }) if failure.class == "GenericError" && failure.desc == "no"
        ));

        assert!(matches!(
            incoming(
                r#"{"timestamp":{"seconds":1,"microseconds":0},"event":"SHUTDOWN","data":{"guest":true}}"#
            ),
            Incoming::Event(event) if event.event == "SHUTDOWN"
        ));
    }

    #[test]
    fn an_error_without_an_id_is_told_apart_from_a_reply() {
        assert!(matches!(
            incoming(r#"{"error":{"class":"GenericError","desc":"JSON parse error"}}"#),
            Incoming::Fault(fault) if fault.error.desc == "JSON parse error"
        ));

        assert!(matches!(
            incoming(r#"{"error":{"class":"GenericError","desc":"JSON parse error"},"id":9}"#),
            Incoming::Reply(Reply { id: 9, .. })
        ));
    }

    #[test]
    fn a_command_without_arguments_carries_none() {
        let command = Command {
            execute: "qmp_capabilities",
            id: 0,
            arguments: Value::Null,
        };

        assert_eq!(
            serde_json::to_string(&command).expect("a serialisable command"),
            r#"{"execute":"qmp_capabilities","id":0}"#
        );

        let command = Command {
            execute: "qom-get",
            id: 1,
            arguments: serde_json::json!({ "property": "size" }),
        };

        assert_eq!(
            serde_json::to_string(&command).expect("a serialisable command"),
            r#"{"execute":"qom-get","id":1,"arguments":{"property":"size"}}"#
        );
    }

    #[test]
    fn a_failure_becomes_the_command_it_refused() {
        let outcome = Outcome::Error(Failure {
            class: "GenericError".to_owned(),
            desc: "block size mismatch".to_owned(),
        });

        let error = outcome
            .into_result("qom-set")
            .expect_err("qemu refused the command");

        assert_eq!(
            error.to_string(),
            "qemu refused qom-set: block size mismatch"
        );
    }
}
