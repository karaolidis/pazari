use std::time::Duration;

use proptest::prelude::*;

use crate::{DenyReason, GrantReason, GuestMessage, HostMessage, PROTOCOL_VERSION, Urgency};

pub fn guest_messages() -> Vec<GuestMessage> {
    vec![
        GuestMessage::Hello {
            protocol: PROTOCOL_VERSION,
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

pub fn host_messages() -> Vec<HostMessage> {
    vec![
        HostMessage::Welcome {
            protocol: PROTOCOL_VERSION,
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

fn grant_reason() -> impl Strategy<Value = GrantReason> {
    prop_oneof![
        Just(GrantReason::Throttle),
        Just(GrantReason::Preflight),
        Just(GrantReason::Manual),
    ]
}

fn urgency() -> impl Strategy<Value = Urgency> {
    prop_oneof![Just(Urgency::Low), Just(Urgency::High)]
}

fn deny_reason() -> impl Strategy<Value = DenyReason> {
    prop_oneof![
        Just(DenyReason::Ceiling),
        Just(DenyReason::HostPressure),
        Just(DenyReason::RateLimited),
        Just(DenyReason::Policy),
    ]
}

fn millis() -> impl Strategy<Value = Duration> {
    any::<u64>().prop_map(Duration::from_millis)
}

pub fn guest_message() -> impl Strategy<Value = GuestMessage> {
    prop_oneof![
        (
            any::<u16>(),
            any::<String>(),
            any::<u64>(),
            any::<u64>(),
            any::<u64>(),
        )
            .prop_map(|(protocol, agent_version, boot_mem, block_size, plugged)| {
                GuestMessage::Hello {
                    protocol,
                    agent_version,
                    boot_mem,
                    block_size,
                    plugged,
                }
            }),
        (
            any::<u64>(),
            any::<u64>(),
            any::<u64>(),
            grant_reason(),
            urgency(),
        )
            .prop_map(|(id, current, target, reason, urgency)| {
                GuestMessage::GrantRequest {
                    id,
                    current,
                    target,
                    reason,
                    urgency,
                }
            }),
        (any::<u64>(), any::<u64>(), any::<u64>()).prop_map(|(id, current, releasable)| {
            GuestMessage::OfferRelease {
                id,
                current,
                releasable,
            }
        }),
        (any::<u64>(), any::<u64>())
            .prop_map(|(id, will_release)| GuestMessage::ReclaimAck { id, will_release }),
        (any::<u64>(), any::<String>())
            .prop_map(|(id, reason)| GuestMessage::ReclaimNak { id, reason }),
        (
            any::<u64>(),
            any::<u64>(),
            any::<u64>(),
            any::<u64>(),
            0.0f32..=100.0,
            0.0f32..=100.0,
            any::<u64>(),
            any::<u64>(),
        )
            .prop_map(
                |(
                    plugged,
                    available,
                    high_events,
                    max_events,
                    psi_some10,
                    psi_full10,
                    swap_used,
                    workload_usage,
                )| GuestMessage::Status {
                    plugged,
                    available,
                    high_events,
                    max_events,
                    psi_some10,
                    psi_full10,
                    swap_used,
                    workload_usage,
                }
            ),
        Just(GuestMessage::Bye {}),
    ]
}

pub fn host_message() -> impl Strategy<Value = HostMessage> {
    prop_oneof![
        (any::<u16>(), any::<u64>(), millis()).prop_map(
            |(protocol, ceiling, status_interval_ms)| {
                HostMessage::Welcome {
                    protocol,
                    ceiling,
                    status_interval_ms,
                }
            }
        ),
        any::<u64>().prop_map(|ceiling| HostMessage::Ceiling { ceiling }),
        (any::<u64>(), any::<u64>()).prop_map(|(id, target)| HostMessage::Grant { id, target }),
        (any::<u64>(), deny_reason(), prop::option::of(millis())).prop_map(
            |(id, reason, retry_after_ms)| HostMessage::Deny {
                id,
                reason,
                retry_after_ms,
            }
        ),
        (any::<u64>(), any::<u64>(), millis()).prop_map(|(id, desired, deadline_ms)| {
            HostMessage::ReclaimRequest {
                id,
                desired,
                deadline_ms,
            }
        }),
    ]
}
