use std::time::Duration;

use pazari_proto::{
    Codec, Decoder, DenyReason, GrantReason, GuestMessage, HostMessage, JsonLinesCodec, Urgency,
};

fn guest_messages() -> Vec<GuestMessage> {
    vec![
        GuestMessage::Hello {
            protocol: pazari_proto::PROTOCOL_VERSION,
            agent_version: "0.1.0".to_owned(),
            boot_mem: 2_147_483_648,
            block_size: 134_217_728,
            plugged: 0,
        },
        GuestMessage::GrantRequest {
            id: 1,
            current: 2_147_483_648,
            target: 4_294_967_296,
            reason: GrantReason::Throttle,
            urgency: Urgency::High,
        },
        GuestMessage::GrantRequest {
            id: 2,
            current: 2_147_483_648,
            target: 3_221_225_472,
            reason: GrantReason::Preflight,
            urgency: Urgency::Low,
        },
        GuestMessage::GrantRequest {
            id: 3,
            current: 2_147_483_648,
            target: 3_221_225_472,
            reason: GrantReason::Manual,
            urgency: Urgency::Low,
        },
        GuestMessage::OfferRelease {
            id: 4,
            current: 4_294_967_296,
            releasable: 1_073_741_824,
        },
        GuestMessage::ReclaimAck {
            id: 7,
            will_release: 536_870_912,
        },
        GuestMessage::ReclaimNak {
            id: 8,
            reason: "workload working set is pinned".to_owned(),
        },
        GuestMessage::Status {
            plugged: 4_294_967_296,
            available: 1_073_741_824,
            high_events: 12,
            max_events: 0,
            psi_some10: 12.5,
            psi_full10: 0.25,
            swap_used: 0,
            workload_usage: 3_221_225_472,
        },
        GuestMessage::Bye {},
    ]
}

fn host_messages() -> Vec<HostMessage> {
    vec![
        HostMessage::Welcome {
            protocol: pazari_proto::PROTOCOL_VERSION,
            ceiling: 8_589_934_592,
            status_interval_ms: Duration::from_millis(1000),
        },
        HostMessage::Ceiling {
            ceiling: 4_294_967_296,
        },
        HostMessage::Grant {
            id: 1,
            target: 3_221_225_472,
        },
        HostMessage::Deny {
            id: 2,
            reason: DenyReason::HostPressure,
            retry_after_ms: Some(Duration::from_millis(5000)),
        },
        HostMessage::Deny {
            id: 3,
            reason: DenyReason::Ceiling,
            retry_after_ms: None,
        },
        HostMessage::Deny {
            id: 4,
            reason: DenyReason::RateLimited,
            retry_after_ms: Some(Duration::from_millis(250)),
        },
        HostMessage::Deny {
            id: 5,
            reason: DenyReason::Policy,
            retry_after_ms: None,
        },
        HostMessage::ReclaimRequest {
            id: 9,
            desired: 1_073_741_824,
            deadline_ms: Duration::from_secs(30),
        },
    ]
}

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

#[test]
fn guest_messages_survive_the_codec() {
    for message in guest_messages() {
        let framed = JsonLinesCodec::encode_to_vec(&message).unwrap();
        assert_eq!(framed.last(), Some(&b'\n'));

        let mut decoder = JsonLinesCodec::decoder::<GuestMessage>();
        decoder.push(&framed).unwrap();

        assert_eq!(decoder.next_message().unwrap(), Some(message));
        assert_eq!(decoder.next_message().unwrap(), None);
    }
}

#[test]
fn host_messages_survive_the_codec() {
    for message in host_messages() {
        let framed = JsonLinesCodec::encode_to_vec(&message).unwrap();
        assert_eq!(framed.last(), Some(&b'\n'));

        let mut decoder = JsonLinesCodec::decoder::<HostMessage>();
        decoder.push(&framed).unwrap();

        assert_eq!(decoder.next_message().unwrap(), Some(message));
        assert_eq!(decoder.next_message().unwrap(), None);
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
