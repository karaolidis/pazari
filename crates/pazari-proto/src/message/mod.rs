use std::{fmt, num::NonZeroU64, time::Duration};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_with::{DurationMilliSeconds, serde_as};

#[cfg(test)]
pub(crate) mod fixtures;

/// Names one grant request or release offer, as the guest numbered it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(pub u64);

impl RequestId {
    /// The id after this one.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Names one reclaim request, as the host numbered it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReclaimId(pub u64);

impl ReclaimId {
    /// The id after this one.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl fmt::Display for ReclaimId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// One reading of the guest's memory.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub total_mem: u64,
    pub available: u64,
    pub high_events: u64,
    pub max_events: u64,
    pub psi_some10: f64,
    pub psi_full10: f64,
    pub swap_used: u64,
    pub workload_usage: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GuestMessage {
    Hello {
        protocol: u16,
        agent_version: String,
        block_size: NonZeroU64,
        total_mem: u64,
    },

    GrantRequest {
        id: RequestId,
        current: u64,
        target: u64,
    },

    OfferRelease {
        id: RequestId,
        current: u64,
        releasable: u64,
    },

    ReclaimAck {
        id: ReclaimId,
        will_release: u64,
    },

    ReclaimNak {
        id: ReclaimId,
        reason: String,
    },

    Status(Status),

    Bye {},
}

impl GuestMessage {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Hello { .. } => "hello",
            Self::GrantRequest { .. } => "grant_request",
            Self::OfferRelease { .. } => "offer_release",
            Self::ReclaimAck { .. } => "reclaim_ack",
            Self::ReclaimNak { .. } => "reclaim_nak",
            Self::Status(_) => "status",
            Self::Bye {} => "bye",
        }
    }
}

#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostMessage {
    Welcome {
        protocol: u16,
        ceiling: u64,
        #[serde_as(as = "DurationMilliSeconds<u64>")]
        #[serde(rename = "status_interval_ms")]
        status_interval: Duration,
        hotplugged: u64,
    },

    Ceiling {
        ceiling: u64,
    },

    Geometry {
        hotplugged: u64,
    },

    Grant {
        id: RequestId,
        target: u64,
    },

    Deny {
        id: RequestId,
        reason: DenyReason,
        #[serde_as(as = "Option<DurationMilliSeconds<u64>>")]
        #[serde(
            rename = "retry_after_ms",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        retry_after: Option<Duration>,
    },

    ReclaimRequest {
        id: ReclaimId,
        desired: u64,
        #[serde_as(as = "DurationMilliSeconds<u64>")]
        #[serde(rename = "deadline_ms")]
        deadline: Duration,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    Ceiling,
    HostPressure,
    RateLimited,
    Policy,
}

impl DenyReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ceiling => "ceiling",
            Self::HostPressure => "host_pressure",
            Self::RateLimited => "rate_limited",
            Self::Policy => "policy",
        }
    }
}

impl fmt::Display for DenyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

pub trait Peer {
    type Incoming: DeserializeOwned;
    type Outgoing: Serialize;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Guest;

impl Peer for Guest {
    type Incoming = HostMessage;
    type Outgoing = GuestMessage;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Host;

impl Peer for Host {
    type Incoming = GuestMessage;
    type Outgoing = HostMessage;
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{DenyReason, GuestMessage, HostMessage, RequestId};
    use crate::{
        Codec, Decoder, JsonLinesCodec,
        testing::{guest_messages, host_messages},
    };

    #[test]
    fn guest_messages_survive_serde() {
        for message in guest_messages() {
            let text = serde_json::to_string(&message).unwrap();
            let back: GuestMessage = serde_json::from_str(&text).unwrap();
            assert_eq!(back, message, "round trip of {text}");
        }
    }

    #[test]
    fn host_messages_survive_serde() {
        for message in host_messages() {
            let text = serde_json::to_string(&message).unwrap();
            let back: HostMessage = serde_json::from_str(&text).unwrap();
            assert_eq!(back, message, "round trip of {text}");
        }
    }

    fn assert_golden<M: serde::Serialize>(message: &M, expected: &str) {
        let framed = JsonLinesCodec::encode_to_vec(message).expect("message encodes");
        let text = String::from_utf8(framed).expect("encoded lines are UTF-8");
        assert_eq!(text, format!("{expected}\n"));
    }

    #[test]
    fn guest_messages_match_their_golden_lines() {
        let messages = guest_messages();
        let expected = [
            r#"{"type":"hello","protocol":1,"agent_version":"0.1.0","block_size":134217728,"total_mem":0}"#,
            r#"{"type":"grant_request","id":1,"current":2147483648,"target":4294967296}"#,
            r#"{"type":"offer_release","id":4,"current":4294967296,"releasable":1073741824}"#,
            r#"{"type":"reclaim_ack","id":7,"will_release":536870912}"#,
            r#"{"type":"reclaim_nak","id":8,"reason":"workload working set is pinned"}"#,
            r#"{"type":"status","total_mem":4294967296,"available":1073741824,"high_events":12,"max_events":0,"psi_some10":12.5,"psi_full10":0.25,"swap_used":0,"workload_usage":3221225472}"#,
            r#"{"type":"bye"}"#,
        ];

        assert_eq!(messages.len(), expected.len());
        for (message, golden) in messages.iter().zip(expected) {
            assert_golden(message, golden);
        }
    }

    #[test]
    fn host_messages_match_their_golden_lines() {
        let messages = host_messages();
        let expected = [
            r#"{"type":"welcome","protocol":1,"ceiling":8589934592,"status_interval_ms":1000,"hotplugged":133656576}"#,
            r#"{"type":"ceiling","ceiling":4294967296}"#,
            r#"{"type":"geometry","hotplugged":133656576}"#,
            r#"{"type":"grant","id":1,"target":3221225472}"#,
            r#"{"type":"deny","id":2,"reason":"host_pressure","retry_after_ms":5000}"#,
            r#"{"type":"deny","id":3,"reason":"ceiling"}"#,
            r#"{"type":"deny","id":4,"reason":"rate_limited","retry_after_ms":250}"#,
            r#"{"type":"deny","id":5,"reason":"policy"}"#,
            r#"{"type":"reclaim_request","id":9,"desired":1073741824,"deadline_ms":30000}"#,
        ];

        assert_eq!(messages.len(), expected.len());
        for (message, golden) in messages.iter().zip(expected) {
            assert_golden(message, golden);
        }
    }

    #[test]
    fn a_deny_reason_spells_itself_as_it_goes_on_the_wire() {
        for reason in [
            DenyReason::Ceiling,
            DenyReason::HostPressure,
            DenyReason::RateLimited,
            DenyReason::Policy,
        ] {
            assert_eq!(
                serde_json::to_value(reason).unwrap(),
                serde_json::Value::from(reason.as_str())
            );
        }
    }

    #[test]
    fn a_guest_message_names_its_kind_as_it_goes_on_the_wire() {
        for message in guest_messages() {
            assert_eq!(
                serde_json::to_value(&message).unwrap()["type"],
                serde_json::Value::from(message.kind())
            );
        }
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let line = br#"{"type":"grant","id":1,"target":42,"quota":7,"note":{"nested":[1,2]}}
"#;

        let mut decoder = JsonLinesCodec::decoder::<HostMessage>();
        decoder.push(line).unwrap();

        assert_eq!(
            decoder.next_message().unwrap(),
            Some(HostMessage::Grant {
                id: RequestId(1),
                target: 42,
            })
        );
    }

    #[test]
    fn retry_after_accepts_absent_null_and_present() {
        let cases = [
            (
                &br#"{"type":"deny","id":1,"reason":"ceiling"}
"#[..],
                None,
            ),
            (
                &br#"{"type":"deny","id":1,"reason":"ceiling","retry_after_ms":null}
"#[..],
                None,
            ),
            (
                &br#"{"type":"deny","id":1,"reason":"ceiling","retry_after_ms":250}
"#[..],
                Some(Duration::from_millis(250)),
            ),
        ];

        for (line, retry_after) in cases {
            let mut decoder = JsonLinesCodec::decoder::<HostMessage>();
            decoder.push(line).unwrap();

            assert_eq!(
                decoder.next_message().unwrap(),
                Some(HostMessage::Deny {
                    id: RequestId(1),
                    reason: DenyReason::Ceiling,
                    retry_after,
                })
            );
        }
    }
}
