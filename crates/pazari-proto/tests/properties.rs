use std::time::Duration;

use pazari_proto::{
    Codec, Decoder, DenyReason, GrantReason, GuestMessage, HostMessage, JsonLinesCodec, Urgency,
};
use proptest::prelude::*;

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

fn guest_message() -> impl Strategy<Value = GuestMessage> {
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

fn host_message() -> impl Strategy<Value = HostMessage> {
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

fn rechunked<M, S>(stream: &[u8], sizes: &[usize]) -> Result<Vec<M>, TestCaseError>
where
    M: serde::de::DeserializeOwned,
    S: Codec,
{
    let mut decoder = S::decoder::<M>();
    let mut decoded = Vec::new();
    let mut offset = 0;

    for size in sizes.iter().copied().cycle() {
        if offset == stream.len() {
            break;
        }

        let end = (offset + size).min(stream.len());
        decoder
            .push(&stream[offset..end])
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        offset = end;

        while let Some(message) = decoder
            .next_message()
            .map_err(|error| TestCaseError::fail(error.to_string()))?
        {
            decoded.push(message);
        }
    }

    Ok(decoded)
}

proptest! {
    #[test]
    fn guest_messages_round_trip(message in guest_message()) {
        let framed = JsonLinesCodec::encode_to_vec(&message)?;
        let decoded = rechunked::<GuestMessage, JsonLinesCodec>(&framed, &[framed.len().max(1)])?;
        prop_assert_eq!(decoded, vec![message]);
    }

    #[test]
    fn host_messages_round_trip(message in host_message()) {
        let framed = JsonLinesCodec::encode_to_vec(&message)?;
        let decoded = rechunked::<HostMessage, JsonLinesCodec>(&framed, &[framed.len().max(1)])?;
        prop_assert_eq!(decoded, vec![message]);
    }

    #[test]
    fn a_guest_stream_survives_arbitrary_chunking(
        messages in prop::collection::vec(guest_message(), 0..8),
        sizes in prop::collection::vec(1usize..64, 1..8),
    ) {
        let mut stream = Vec::new();
        for message in &messages {
            JsonLinesCodec::encode(message, &mut stream)?;
        }

        let decoded = rechunked::<GuestMessage, JsonLinesCodec>(&stream, &sizes)?;
        prop_assert_eq!(decoded, messages);
    }

    #[test]
    fn a_host_stream_survives_arbitrary_chunking(
        messages in prop::collection::vec(host_message(), 0..8),
        sizes in prop::collection::vec(1usize..64, 1..8),
    ) {
        let mut stream = Vec::new();
        for message in &messages {
            JsonLinesCodec::encode(message, &mut stream)?;
        }

        let decoded = rechunked::<HostMessage, JsonLinesCodec>(&stream, &sizes)?;
        prop_assert_eq!(decoded, messages);
    }
}
