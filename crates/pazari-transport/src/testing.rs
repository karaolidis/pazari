use std::{
    path::PathBuf,
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

use pazari_proto::{
    Codec, DenyReason, GrantReason, GuestMessage, HostMessage, JsonLinesCodec, PROTOCOL_VERSION,
    Status, Urgency,
};

pub use crate::{GuestStream, HostStream};

static SEQUENCE: AtomicU32 = AtomicU32::new(0);

pub fn socket_path(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "pazari-transport-{}-{name}-{}.sock",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    path
}

pub fn guest_messages() -> Vec<GuestMessage> {
    vec![
        GuestMessage::Hello {
            protocol: PROTOCOL_VERSION,
            agent_version: "0.1.0".to_owned(),
            boot_mem: 2_147_483_648,
            block_size: 2_097_152,
            plugged: 0,
        },
        GuestMessage::GrantRequest {
            id: 1,
            current: 2_147_483_648,
            target: 4_294_967_296,
            reason: GrantReason::Throttle,
            urgency: Urgency::High,
        },
        GuestMessage::Status(Status {
            plugged: 2_147_483_648,
            available: 536_870_912,
            high_events: 17,
            max_events: 0,
            psi_some10: 12.5,
            psi_full10: 0.25,
            swap_used: 0,
            workload_usage: 1_610_612_736,
        }),
        GuestMessage::Bye {},
    ]
}

pub fn host_messages() -> Vec<HostMessage> {
    vec![
        HostMessage::Welcome {
            protocol: PROTOCOL_VERSION,
            ceiling: 8_589_934_592,
            status_interval_ms: Duration::from_millis(2_500),
        },
        HostMessage::Ceiling {
            ceiling: 6_442_450_944,
        },
        HostMessage::Deny {
            id: 1,
            reason: DenyReason::RateLimited,
            retry_after_ms: Some(Duration::from_millis(750)),
        },
        HostMessage::ReclaimRequest {
            id: 9,
            desired: 1_073_741_824,
            deadline_ms: Duration::from_secs(30),
        },
    ]
}

pub fn framed<M: serde::Serialize>(messages: &[M]) -> Vec<u8> {
    let mut out = Vec::new();
    for message in messages {
        JsonLinesCodec::encode(message, &mut out).expect("failed to encode a message");
    }
    out
}
