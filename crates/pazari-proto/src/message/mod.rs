use std::time::Duration;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_with::{DurationMilliSeconds, serde_as};

#[cfg(test)]
pub(crate) mod fixtures;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GuestMessage {
    Hello {
        protocol: u16,
        agent_version: String,
        boot_mem: u64,
        block_size: u64,
        plugged: u64,
    },

    GrantRequest {
        id: u64,
        current: u64,
        target: u64,
        reason: GrantReason,
        urgency: Urgency,
    },

    OfferRelease {
        id: u64,
        current: u64,
        releasable: u64,
    },

    ReclaimAck {
        id: u64,
        will_release: u64,
    },

    ReclaimNak {
        id: u64,
        reason: String,
    },

    Status {
        plugged: u64,
        available: u64,
        high_events: u64,
        max_events: u64,
        psi_some10: f32,
        psi_full10: f32,
        swap_used: u64,
        workload_usage: u64,
    },

    Bye {},
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantReason {
    Throttle,
    Preflight,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Urgency {
    Low,
    High,
}

#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostMessage {
    Welcome {
        protocol: u16,
        ceiling: u64,
        #[serde_as(as = "DurationMilliSeconds<u64>")]
        status_interval_ms: Duration,
    },

    Ceiling {
        ceiling: u64,
    },

    Grant {
        id: u64,
        target: u64,
    },

    Deny {
        id: u64,
        reason: DenyReason,
        #[serde_as(as = "Option<DurationMilliSeconds<u64>>")]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after_ms: Option<Duration>,
    },

    ReclaimRequest {
        id: u64,
        desired: u64,
        #[serde_as(as = "DurationMilliSeconds<u64>")]
        deadline_ms: Duration,
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

    use super::{
        DenyReason, GuestMessage, HostMessage,
        fixtures::{guest_messages, host_messages},
    };
    use crate::{Codec, Decoder, JsonLinesCodec};

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
            r#"{"type":"hello","protocol":1,"agent_version":"0.1.0","boot_mem":2147483648,"block_size":134217728,"plugged":0}"#,
            r#"{"type":"grant_request","id":1,"current":2147483648,"target":4294967296,"reason":"throttle","urgency":"high"}"#,
            r#"{"type":"grant_request","id":2,"current":2147483648,"target":3221225472,"reason":"preflight","urgency":"low"}"#,
            r#"{"type":"grant_request","id":3,"current":2147483648,"target":3221225472,"reason":"manual","urgency":"low"}"#,
            r#"{"type":"offer_release","id":4,"current":4294967296,"releasable":1073741824}"#,
            r#"{"type":"reclaim_ack","id":7,"will_release":536870912}"#,
            r#"{"type":"reclaim_nak","id":8,"reason":"workload working set is pinned"}"#,
            r#"{"type":"status","plugged":4294967296,"available":1073741824,"high_events":12,"max_events":0,"psi_some10":12.5,"psi_full10":0.25,"swap_used":0,"workload_usage":3221225472}"#,
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
            r#"{"type":"welcome","protocol":1,"ceiling":8589934592,"status_interval_ms":1000}"#,
            r#"{"type":"ceiling","ceiling":4294967296}"#,
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
    fn unknown_fields_are_ignored() {
        let line = br#"{"type":"grant","id":1,"target":42,"quota":7,"note":{"nested":[1,2]}}
"#;

        let mut decoder = JsonLinesCodec::decoder::<HostMessage>();
        decoder.push(line).unwrap();

        assert_eq!(
            decoder.next_message().unwrap(),
            Some(HostMessage::Grant { id: 1, target: 42 })
        );
    }

    #[test]
    fn retry_after_ms_accepts_absent_null_and_present() {
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

        for (line, retry_after_ms) in cases {
            let mut decoder = JsonLinesCodec::decoder::<HostMessage>();
            decoder.push(line).unwrap();

            assert_eq!(
                decoder.next_message().unwrap(),
                Some(HostMessage::Deny {
                    id: 1,
                    reason: DenyReason::Ceiling,
                    retry_after_ms,
                })
            );
        }
    }
}
