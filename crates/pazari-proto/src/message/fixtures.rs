use std::{num::NonZeroU64, time::Duration};

use proptest::prelude::*;

use crate::{DenyReason, GuestMessage, HostMessage, ReclaimId, RequestId, Status};

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
        (any::<u16>(), any::<String>(), 1..=u64::MAX, any::<u64>()).prop_map(
            |(protocol, agent_version, block_size, total_mem)| {
                GuestMessage::Hello {
                    protocol,
                    agent_version,
                    block_size: NonZeroU64::new(block_size).expect("a positive block size"),
                    total_mem,
                }
            }
        ),
        (any::<u64>(), any::<u64>(), any::<u64>()).prop_map(|(id, current, target)| {
            GuestMessage::GrantRequest {
                id: RequestId(id),
                current,
                target,
            }
        }),
        (any::<u64>(), any::<u64>(), any::<u64>()).prop_map(|(id, current, releasable)| {
            GuestMessage::OfferRelease {
                id: RequestId(id),
                current,
                releasable,
            }
        }),
        (any::<u64>(), any::<u64>()).prop_map(|(id, will_release)| GuestMessage::ReclaimAck {
            id: ReclaimId(id),
            will_release,
        }),
        (any::<u64>(), any::<String>()).prop_map(|(id, reason)| GuestMessage::ReclaimNak {
            id: ReclaimId(id),
            reason,
        }),
        (
            any::<u64>(),
            any::<u64>(),
            any::<u64>(),
            any::<u64>(),
            0.0f64..=100.0,
            0.0f64..=100.0,
            any::<u64>(),
            any::<u64>(),
        )
            .prop_map(
                |(
                    total_mem,
                    available,
                    high_events,
                    max_events,
                    psi_some10,
                    psi_full10,
                    swap_used,
                    workload_usage,
                )| GuestMessage::Status(Status {
                    total_mem,
                    available,
                    high_events,
                    max_events,
                    psi_some10,
                    psi_full10,
                    swap_used,
                    workload_usage,
                })
            ),
        Just(GuestMessage::Bye {}),
    ]
}

pub fn host_message() -> impl Strategy<Value = HostMessage> {
    prop_oneof![
        (any::<u16>(), any::<u64>(), millis(), any::<u64>()).prop_map(
            |(protocol, ceiling, status_interval, hotplugged)| {
                HostMessage::Welcome {
                    protocol,
                    ceiling,
                    status_interval,
                    hotplugged,
                }
            }
        ),
        any::<u64>().prop_map(|ceiling| HostMessage::Ceiling { ceiling }),
        any::<u64>().prop_map(|hotplugged| HostMessage::Geometry { hotplugged }),
        (any::<u64>(), any::<u64>()).prop_map(|(id, target)| HostMessage::Grant {
            id: RequestId(id),
            target,
        }),
        (any::<u64>(), deny_reason(), prop::option::of(millis())).prop_map(
            |(id, reason, retry_after)| HostMessage::Deny {
                id: RequestId(id),
                reason,
                retry_after,
            }
        ),
        (any::<u64>(), any::<u64>(), millis()).prop_map(|(id, desired, deadline)| {
            HostMessage::ReclaimRequest {
                id: ReclaimId(id),
                desired,
                deadline,
            }
        }),
    ]
}
