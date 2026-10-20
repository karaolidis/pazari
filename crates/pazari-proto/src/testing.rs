//! Messages of every kind, for tests in this crate and the ones that carry them.

use std::{num::NonZeroU64, time::Duration};

use crate::{
    DenyReason, GuestMessage, HostMessage, PROTOCOL_VERSION, ReclaimId, RequestId, Status,
};

#[must_use]
pub fn guest_messages() -> Vec<GuestMessage> {
    vec![
        GuestMessage::Hello {
            protocol: PROTOCOL_VERSION,
            agent_version: "0.1.0".to_owned(),
            block_size: NonZeroU64::new(134_217_728).expect("a positive block size"),
            total_mem: 0,
        },
        GuestMessage::GrantRequest {
            id: RequestId(1),
            current: 2_147_483_648,
            target: 4_294_967_296,
        },
        GuestMessage::OfferRelease {
            id: RequestId(4),
            current: 4_294_967_296,
            releasable: 1_073_741_824,
        },
        GuestMessage::ReclaimAck {
            id: ReclaimId(7),
            will_release: 536_870_912,
        },
        GuestMessage::ReclaimNak {
            id: ReclaimId(8),
            reason: "workload working set is pinned".to_owned(),
        },
        GuestMessage::Status(Status {
            total_mem: 4_294_967_296,
            available: 1_073_741_824,
            high_events: 12,
            max_events: 0,
            psi_some10: 12.5,
            psi_full10: 0.25,
            swap_used: 0,
            workload_usage: 3_221_225_472,
        }),
        GuestMessage::Bye {},
    ]
}

#[must_use]
pub fn host_messages() -> Vec<HostMessage> {
    vec![
        HostMessage::Welcome {
            protocol: PROTOCOL_VERSION,
            ceiling: 8_589_934_592,
            status_interval: Duration::from_millis(1000),
            hotplugged: 133_656_576,
        },
        HostMessage::Ceiling {
            ceiling: 4_294_967_296,
        },
        HostMessage::Geometry {
            hotplugged: 133_656_576,
        },
        HostMessage::Grant {
            id: RequestId(1),
            target: 3_221_225_472,
        },
        HostMessage::Deny {
            id: RequestId(2),
            reason: DenyReason::HostPressure,
            retry_after: Some(Duration::from_millis(5000)),
        },
        HostMessage::Deny {
            id: RequestId(3),
            reason: DenyReason::Ceiling,
            retry_after: None,
        },
        HostMessage::Deny {
            id: RequestId(4),
            reason: DenyReason::RateLimited,
            retry_after: Some(Duration::from_millis(250)),
        },
        HostMessage::Deny {
            id: RequestId(5),
            reason: DenyReason::Policy,
            retry_after: None,
        },
        HostMessage::ReclaimRequest {
            id: ReclaimId(9),
            desired: 1_073_741_824,
            deadline: Duration::from_secs(30),
        },
    ]
}
