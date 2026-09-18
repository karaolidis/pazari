use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_with::{DurationMilliSeconds, serde_as};

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
