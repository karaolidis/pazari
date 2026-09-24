pub mod action;
pub mod arbiter;
pub mod context;
pub mod event;
pub mod guest;
pub mod phase;
pub mod policy;
pub mod registry;
pub mod session;

mod handle;

#[cfg(test)]
pub(crate) mod testing;

use pazari_transport::PeerId;

pub use crate::broker::{
    action::Action,
    arbiter::{Arbiter, Ask, Decision, Fcfs, GuestView, HostView, Request},
    context::Context,
    event::Event,
    guest::{Entry, Guest, Guests, Reconcile},
    phase::{AwaitingHello, Negotiated, Phase, Reclaim, Said, State},
    policy::{Policy, Revocation},
    registry::{GuestId, GuestSpec, Registry, RegistryError},
    session::Session,
};

#[derive(Debug)]
pub struct Broker {
    ctx: Context,
}

impl Broker {
    #[must_use]
    pub fn new(policy: Policy, guests: Registry) -> Self {
        Self::with_arbiter(policy, guests, Fcfs)
    }

    #[must_use]
    pub fn with_arbiter(
        policy: Policy,
        registry: Registry,
        arbiter: impl Arbiter + 'static,
    ) -> Self {
        Self {
            ctx: Context::new(policy, registry, arbiter),
        }
    }

    #[must_use]
    pub fn handle(&mut self, event: Event) -> Vec<Action> {
        match event {
            Event::Connected { peer, .. } => self.on_connected(peer),
            Event::Message { now, peer, message } => self.on_message(now, &peer, message),
            Event::Disconnected { peer, .. } => self.on_disconnected(&peer),
            Event::Size {
                now,
                guest,
                size,
                hotplugged,
            } => self.on_size(now, guest, size, hotplugged),
            Event::HostAvailable {
                bytes,
                swap_free,
                psi,
                ..
            } => {
                self.ctx.host = HostView {
                    available: Some(bytes),
                    swap_free: Some(swap_free),
                    psi,
                };
            }
            Event::Reclaim {
                now,
                guest,
                desired,
            } => self.on_reclaim(now, guest, desired),
            Event::Tick { now } => self.on_tick(now),
        }

        self.ctx.drain()
    }

    #[must_use]
    pub const fn policy(&self) -> &Policy {
        &self.ctx.policy
    }

    #[must_use]
    pub const fn guests(&self) -> &Guests {
        &self.ctx.guests
    }

    #[must_use]
    pub fn guest(&self, guest: GuestId) -> Option<&Guest> {
        self.ctx.guests.state(guest)
    }

    #[must_use]
    pub const fn host(&self) -> HostView {
        self.ctx.host
    }

    #[must_use]
    pub const fn host_available(&self) -> Option<u64> {
        self.ctx.host.available
    }

    #[must_use]
    pub fn is_negotiated(&self, peer: &PeerId) -> bool {
        self.ctx
            .guests
            .lookup(peer)
            .and_then(|guest| self.ctx.guests.negotiated(guest))
            .is_some()
    }

    #[cfg(test)]
    fn negotiated(&self, guest: GuestId) -> Option<&Negotiated> {
        self.ctx.guests.negotiated(guest)
    }

    #[must_use]
    pub fn is_stale(&self, guest: GuestId) -> bool {
        self.ctx
            .guests
            .negotiated(guest)
            .is_some_and(|session| session.stale)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pazari_proto::{DenyReason, HostMessage, PROTOCOL_VERSION};
    use pazari_types::Timestamp;

    use pazari_proto::GuestMessage;
    use proptest::prelude::*;

    use super::*;
    use crate::broker::testing::*;

    #[test]
    fn a_hello_is_answered_with_the_ceiling_and_the_cadence() {
        let mut broker = sized();

        assert_eq!(connect(&mut broker, at(0), ONE), vec![]);
        assert_eq!(
            message(&mut broker, at(0), ONE, hello()),
            vec![Action::Send {
                peer: vsock(ONE),
                message: HostMessage::Welcome {
                    protocol: PROTOCOL_VERSION,
                    ceiling: CEILING,
                    status_interval_ms: Duration::from_secs(1),
                    hotplugged: 0,
                },
            }]
        );
        assert!(broker.is_negotiated(&vsock(ONE)));
    }

    #[test]
    fn a_guest_speaking_another_protocol_is_shown_the_door() {
        let mut broker = sized();

        connect(&mut broker, at(0), ONE);

        let wrong = GuestMessage::Hello {
            protocol: PROTOCOL_VERSION + 1,
            agent_version: "0.1.0".to_owned(),
            block_size: BLOCK,
            total_mem: BOOT,
        };

        assert_eq!(message(&mut broker, at(0), ONE, wrong), vec![closed(ONE)]);
        assert!(!broker.is_negotiated(&vsock(ONE)));
    }

    #[test]
    fn a_guest_declaring_an_impossible_geometry_is_shown_the_door() {
        let mut broker = sized();

        connect(&mut broker, at(0), ONE);

        let wrong = GuestMessage::Hello {
            protocol: PROTOCOL_VERSION,
            agent_version: "0.1.0".to_owned(),
            block_size: 0,
            total_mem: BOOT,
        };

        assert_eq!(message(&mut broker, at(0), ONE, wrong), vec![closed(ONE)]);
    }

    #[test]
    fn a_guest_speaking_before_it_says_hello_is_shown_the_door() {
        let mut broker = sized();

        connect(&mut broker, at(0), ONE);

        assert_eq!(
            message(&mut broker, at(0), ONE, ask(1, BOOT, 3072 * MIB)),
            vec![closed(ONE)]
        );
        assert!(!broker.is_negotiated(&vsock(ONE)));
    }

    #[test]
    fn a_peer_no_guest_answers_to_is_shown_the_door() {
        let mut broker = sized();

        assert_eq!(connect(&mut broker, at(0), 99), vec![closed(99)]);
        assert_eq!(message(&mut broker, at(0), 99, hello()), vec![closed(99)]);
    }

    #[test]
    fn a_message_on_a_connection_the_broker_never_accepted_is_shown_the_door() {
        let mut broker = sized();

        assert_eq!(message(&mut broker, at(0), ONE, hello()), vec![closed(ONE)]);
    }

    #[test]
    fn a_second_hello_is_shown_the_door() {
        let mut broker = welcomed(ONE);

        assert_eq!(message(&mut broker, at(1), ONE, hello()), vec![closed(ONE)]);
        assert!(!broker.is_negotiated(&vsock(ONE)));
    }

    #[test]
    fn a_grant_inside_the_ceiling_and_over_the_floor_is_actuated() {
        let mut broker = welcomed(ONE);

        assert_eq!(
            message(&mut broker, at(10), ONE, ask(1, BOOT, 3072 * MIB)),
            vec![grant(ONE, 1, 3072 * MIB), requested(ONE, 3072 * MIB),]
        );
        assert_eq!(
            broker.guest(id(ONE)).and_then(Guest::requested),
            Some(3072 * MIB)
        );
    }

    #[test]
    fn a_request_over_the_ceiling_is_granted_the_ceiling() {
        let mut broker = welcomed(ONE);

        assert_eq!(
            message(&mut broker, at(10), ONE, ask(1, BOOT, 8192 * MIB)),
            vec![grant(ONE, 1, CEILING), requested(ONE, CEILING)]
        );
    }

    #[test]
    fn a_guest_already_at_its_ceiling_is_denied() {
        let mut broker = welcomed(ONE);

        sizes(&mut broker, at(5), ONE, CEILING);

        assert_eq!(
            message(&mut broker, at(10), ONE, ask(1, CEILING, 8192 * MIB)),
            vec![deny(ONE, 1, DenyReason::Ceiling)]
        );
        assert_eq!(broker.guest(id(ONE)).and_then(Guest::requested), None);
    }

    const RESERVED: u64 = 136 * MIB;

    fn reserving() -> Broker {
        let mut broker = sized();

        greet_at(&mut broker, at(0), ONE, BOOT - RESERVED, 0);

        broker
    }

    #[test]
    fn a_guest_asking_for_what_its_device_already_holds_is_still_grown() {
        let mut broker = reserving();

        assert_eq!(
            message(&mut broker, at(10), ONE, ask(1, BOOT - RESERVED, BOOT)),
            vec![
                grant(ONE, 1, BOOT + 120 * MIB),
                requested(ONE, BOOT + 256 * MIB)
            ]
        );
    }

    #[test]
    fn a_grow_carries_the_guest_past_what_it_asked_for_never_short_of_it() {
        let mut broker = reserving();

        let wanted = BOOT + 256 * MIB;

        let Action::Send {
            message: HostMessage::Grant { target, .. },
            ..
        } = message(&mut broker, at(10), ONE, ask(1, BOOT - RESERVED, wanted))
            .into_iter()
            .next()
            .expect("an answer")
        else {
            panic!("the guest was not granted its grow");
        };

        assert!(target >= wanted, "{target} is short of {wanted}");
        assert!(target - wanted < BLOCK, "{target} overshoots {wanted}");
    }

    #[test]
    fn the_offset_is_taken_again_from_every_status() {
        let mut broker = reserving();

        let _ = message(&mut broker, at(10), ONE, ask(1, BOOT - RESERVED, BOOT));
        sizes(&mut broker, at(11), ONE, BOOT + 256 * MIB);

        let _ = message(
            &mut broker,
            at(12),
            ONE,
            GuestMessage::Status(status(BOOT + 64 * MIB, 512 * MIB)),
        );

        assert_eq!(
            broker.guest(id(ONE)).and_then(|guest| guest.offset),
            Some(192 * MIB)
        );

        assert_eq!(
            message(
                &mut broker,
                at(20),
                ONE,
                ask(2, BOOT + 64 * MIB, BOOT + 192 * MIB)
            ),
            vec![
                grant(ONE, 2, BOOT + 192 * MIB),
                requested(ONE, BOOT + 384 * MIB)
            ]
        );
    }

    #[test]
    fn an_offset_is_not_taken_while_a_resize_is_in_flight() {
        let mut broker = reserving();

        let _ = message(&mut broker, at(10), ONE, ask(1, BOOT - RESERVED, BOOT));

        let _ = message(
            &mut broker,
            at(11),
            ONE,
            GuestMessage::Status(status(BOOT - RESERVED, 512 * MIB)),
        );

        assert_eq!(
            broker.guest(id(ONE)).and_then(|guest| guest.offset),
            Some(RESERVED)
        );
    }

    #[test]
    fn a_guest_that_has_not_said_what_it_sees_is_answered_in_the_device_s_terms() {
        let mut broker = held_guest();

        sizes(&mut broker, at(5), ONE, BOOT);

        assert_eq!(broker.guest(id(ONE)).and_then(|guest| guest.offset), None);

        assert_eq!(
            message(&mut broker, at(10), ONE, ask(1, BOOT, 3072 * MIB)),
            vec![grant(ONE, 1, 3072 * MIB), requested(ONE, 3072 * MIB)]
        );
    }

    fn held_guest() -> Broker {
        let mut broker = Broker::new(policy(), registry());

        let _ = broker.handle(available(at(0), HOST));

        connect(&mut broker, at(0), ONE);

        assert_eq!(
            message(&mut broker, at(0), ONE, hello()),
            vec![],
            "the host answered before it knew how much of the guest was plugged in",
        );

        broker
    }

    #[test]
    fn a_hello_is_held_until_the_guest_device_reports_a_size() {
        let mut broker = held_guest();

        assert!(!broker.is_negotiated(&vsock(ONE)));

        assert_eq!(
            sizes(&mut broker, at(11), ONE, 2560 * MIB),
            vec![welcome(ONE, CEILING, 2560 * MIB - BOOT)],
            "the welcome did not follow the size that unblocked it",
        );
        assert!(broker.is_negotiated(&vsock(ONE)));
    }

    #[test]
    fn a_held_guest_starts_its_status_clock_when_it_is_welcomed() {
        let mut broker = held_guest();

        sizes(&mut broker, at(60), ONE, BOOT);
        let _ = tick(&mut broker, at(61));

        assert!(
            !broker.is_stale(id(ONE)),
            "a guest was stale before it had a chance to say anything",
        );
    }

    #[test]
    fn a_guest_held_at_hello_negotiates_once_the_welcome_lands() {
        let mut broker = held_guest();

        sizes(&mut broker, at(11), ONE, BOOT);

        assert_eq!(
            message(&mut broker, at(12), ONE, ask(1, BOOT, 3072 * MIB)),
            vec![grant(ONE, 1, 3072 * MIB), requested(ONE, 3072 * MIB)]
        );
    }

    #[test]
    fn a_guest_that_speaks_while_its_hello_is_held_is_hung_up_on() {
        let mut broker = held_guest();

        assert_eq!(
            message(&mut broker, at(10), ONE, ask(1, BOOT, 3072 * MIB)),
            vec![closed(ONE)]
        );
        assert_eq!(broker.guest(id(ONE)).and_then(Guest::requested), None);
    }

    #[test]
    fn a_guest_that_says_hello_twice_while_it_is_held_is_hung_up_on() {
        let mut broker = held_guest();

        assert_eq!(
            message(&mut broker, at(10), ONE, hello()),
            vec![closed(ONE)]
        );
    }

    #[test]
    fn a_grant_that_would_take_the_host_under_its_floor_is_denied() {
        let mut broker = welcomed(ONE);

        let _ = broker.handle(available(at(9), FLOOR + 512 * MIB));

        assert_eq!(
            message(&mut broker, at(10), ONE, ask(1, BOOT, 3072 * MIB)),
            vec![retry(
                ONE,
                1,
                DenyReason::HostPressure,
                Duration::from_secs(5),
            )]
        );
    }

    #[test]
    fn a_guest_that_just_grew_is_told_to_wait() {
        let mut broker = welcomed(ONE);

        message(&mut broker, at(10), ONE, ask(1, BOOT, 2560 * MIB));

        assert_eq!(
            message(&mut broker, at(12), ONE, ask(2, BOOT, 3072 * MIB)),
            vec![retry(
                ONE,
                2,
                DenyReason::RateLimited,
                Duration::from_secs(3),
            )]
        );

        sizes(&mut broker, at(13), ONE, 2560 * MIB);

        assert_eq!(
            message(&mut broker, at(15), ONE, ask(3, 2560 * MIB, 3072 * MIB)),
            vec![grant(ONE, 3, 3072 * MIB), requested(ONE, 3072 * MIB)]
        );
    }

    #[test]
    fn an_offer_to_release_is_taken_and_actuated_downward() {
        let mut broker = plugged(ONE);

        assert_eq!(
            message(&mut broker, at(10), ONE, release(1, BOOT + GIB, 512 * MIB)),
            vec![
                grant(ONE, 1, BOOT + 512 * MIB),
                requested(ONE, BOOT + 512 * MIB),
            ]
        );
    }

    #[test]
    fn an_offer_of_more_than_is_plugged_in_stops_at_the_boot_memory() {
        let mut broker = plugged(ONE);

        assert_eq!(
            message(&mut broker, at(10), ONE, release(1, BOOT + GIB, u64::MAX)),
            vec![grant(ONE, 1, BOOT), requested(ONE, BOOT)]
        );
    }

    #[test]
    fn a_release_does_not_arm_the_rate_limit() {
        let mut broker = plugged(ONE);

        message(&mut broker, at(10), ONE, release(1, BOOT + GIB, 512 * MIB));
        sizes(&mut broker, at(11), ONE, BOOT + 512 * MIB);

        assert_eq!(
            message(
                &mut broker,
                at(12),
                ONE,
                ask(2, BOOT + 512 * MIB, BOOT + GIB)
            ),
            vec![grant(ONE, 2, BOOT + GIB), requested(ONE, BOOT + GIB)]
        );
    }

    #[test]
    fn a_request_id_that_does_not_climb_is_shown_the_door() {
        let mut broker = welcomed(ONE);

        message(&mut broker, at(10), ONE, ask(4, BOOT, 2560 * MIB));

        assert_eq!(
            message(&mut broker, at(11), ONE, ask(4, BOOT, 2560 * MIB)),
            vec![closed(ONE)]
        );
        assert!(!broker.is_negotiated(&vsock(ONE)));
    }

    #[test]
    fn an_actuation_is_over_once_the_size_reports_it() {
        let mut broker = welcomed(ONE);

        message(&mut broker, at(10), ONE, ask(1, BOOT, 3072 * MIB));

        assert_eq!(
            sizes(&mut broker, at(11), ONE, 2560 * MIB),
            vec![geometry(ONE, 512 * MIB)]
        );
        assert_eq!(
            broker.guest(id(ONE)).and_then(Guest::requested),
            Some(3072 * MIB)
        );

        assert_eq!(
            sizes(&mut broker, at(12), ONE, 3072 * MIB),
            vec![geometry(ONE, GIB)]
        );
        assert_eq!(broker.guest(id(ONE)).and_then(Guest::requested), None);
        assert_eq!(
            broker.guest(id(ONE)).and_then(|guest| guest.size),
            Some(3072 * MIB)
        );
    }

    #[test]
    fn a_grow_that_stalls_settles_for_what_arrived() {
        let mut broker = welcomed(ONE);

        message(&mut broker, at(10), ONE, ask(1, BOOT, 3072 * MIB));
        sizes(&mut broker, at(11), ONE, 2560 * MIB);

        assert_eq!(tick(&mut broker, at(39)), vec![]);
        assert_eq!(
            broker.guest(id(ONE)).and_then(Guest::requested),
            Some(3072 * MIB)
        );

        assert_eq!(tick(&mut broker, at(40)), vec![]);
        assert_eq!(broker.guest(id(ONE)).and_then(Guest::requested), None);
        assert_eq!(
            broker.guest(id(ONE)).and_then(|guest| guest.size),
            Some(2560 * MIB)
        );
    }

    #[test]
    fn a_shrink_that_stalls_is_taken_back_to_what_the_guest_has() {
        let mut broker = plugged(ONE);

        message(&mut broker, at(10), ONE, release(1, BOOT + GIB, 512 * MIB));

        assert_eq!(tick(&mut broker, at(39)), vec![]);
        assert_eq!(tick(&mut broker, at(40)), vec![requested(ONE, BOOT + GIB)]);
        assert_eq!(broker.guest(id(ONE)).and_then(Guest::requested), None);
        assert_eq!(tick(&mut broker, at(41)), vec![]);
    }

    #[test]
    fn a_shrink_that_stalls_is_left_pending_where_policy_says_so() {
        let policy = Policy {
            shrink_revert: false,
            ..policy()
        };

        let mut broker = sized_with(policy);

        greet(&mut broker, at(0), ONE);
        sizes(&mut broker, at(1), ONE, BOOT + GIB);

        message(&mut broker, at(10), ONE, release(1, BOOT + GIB, 512 * MIB));

        assert_eq!(tick(&mut broker, at(40)), vec![]);
        assert_eq!(broker.guest(id(ONE)).and_then(Guest::requested), None);
    }

    #[test]
    fn a_guest_that_goes_quiet_is_marked_stale() {
        let mut broker = welcomed(ONE);

        assert_eq!(tick(&mut broker, at(3)), vec![]);
        assert!(!broker.is_stale(id(ONE)));

        assert_eq!(tick(&mut broker, at(4)), vec![]);
        assert!(broker.is_stale(id(ONE)));

        message(&mut broker, at(5), ONE, report(BOOT, 1024 * MIB));

        assert!(!broker.is_stale(id(ONE)));
        assert_eq!(
            broker
                .guest(id(ONE))
                .and_then(|guest| guest.session.as_ref())
                .and_then(Session::negotiated)
                .and_then(|session| session.status),
            Some(status(BOOT, 1024 * MIB))
        );
    }

    #[test]
    fn a_stale_guest_is_still_answered() {
        let mut broker = welcomed(ONE);

        tick(&mut broker, at(10));
        assert!(broker.is_stale(id(ONE)));

        assert_eq!(
            message(&mut broker, at(11), ONE, ask(1, BOOT, 3072 * MIB)),
            vec![grant(ONE, 1, 3072 * MIB), requested(ONE, 3072 * MIB)]
        );
    }

    #[test]
    fn a_reclaim_request_carries_its_own_climbing_ids() {
        let mut broker = welcomed(ONE);

        assert_eq!(
            broker.handle(Event::Reclaim {
                now: at(10),
                guest: id(ONE),
                desired: 512 * MIB,
            }),
            vec![Action::Send {
                peer: vsock(ONE),
                message: HostMessage::ReclaimRequest {
                    id: 1,
                    desired: 512 * MIB,
                    deadline_ms: Duration::from_secs(30),
                },
            }]
        );

        assert_eq!(
            broker.handle(Event::Reclaim {
                now: at(11),
                guest: id(ONE),
                desired: 512 * MIB,
            }),
            vec![]
        );

        message(
            &mut broker,
            at(12),
            ONE,
            GuestMessage::ReclaimNak {
                id: 1,
                reason: "the workload is pinned".to_owned(),
            },
        );

        assert_eq!(
            broker.handle(Event::Reclaim {
                now: at(13),
                guest: id(ONE),
                desired: 256 * MIB,
            }),
            vec![],
            "a refusal was taken as leave to ask again at once",
        );

        tick(&mut broker, at(40));

        assert_eq!(
            broker.handle(Event::Reclaim {
                now: at(41),
                guest: id(ONE),
                desired: 256 * MIB,
            }),
            vec![Action::Send {
                peer: vsock(ONE),
                message: HostMessage::ReclaimRequest {
                    id: 2,
                    desired: 256 * MIB,
                    deadline_ms: Duration::from_secs(30),
                },
            }]
        );
    }

    #[test]
    fn a_reclaim_the_guest_takes_on_waits_for_the_memory() {
        let mut broker = plugged(ONE);

        let _ = broker.handle(Event::Reclaim {
            now: at(10),
            guest: id(ONE),
            desired: 512 * MIB,
        });

        assert_eq!(
            message(
                &mut broker,
                at(11),
                ONE,
                GuestMessage::ReclaimAck {
                    id: 1,
                    will_release: 256 * MIB,
                }
            ),
            vec![]
        );
        assert_eq!(
            broker
                .negotiated(id(ONE))
                .and_then(|session| session.reclaim)
                .and_then(|reclaim| reclaim.committed),
            Some(256 * MIB)
        );

        assert_eq!(
            message(&mut broker, at(12), ONE, release(1, BOOT + GIB, 256 * MIB)),
            vec![
                grant(ONE, 1, BOOT + 768 * MIB),
                requested(ONE, BOOT + 768 * MIB),
            ]
        );
    }

    #[test]
    fn a_reclaim_that_runs_out_of_time_is_given_up_on() {
        let mut broker = welcomed(ONE);

        let _ = broker.handle(Event::Reclaim {
            now: at(10),
            guest: id(ONE),
            desired: 512 * MIB,
        });

        assert_eq!(tick(&mut broker, at(39)), vec![]);
        assert!(
            broker
                .negotiated(id(ONE))
                .and_then(|session| session.reclaim)
                .is_some()
        );

        assert_eq!(tick(&mut broker, at(40)), vec![]);
        assert!(
            broker
                .negotiated(id(ONE))
                .and_then(|session| session.reclaim)
                .is_none()
        );
    }

    #[test]
    fn an_answer_to_a_reclaim_the_host_never_asked_for_is_shown_the_door() {
        let mut broker = welcomed(ONE);

        assert_eq!(
            message(
                &mut broker,
                at(10),
                ONE,
                GuestMessage::ReclaimAck {
                    id: 7,
                    will_release: 256 * MIB,
                }
            ),
            vec![closed(ONE)]
        );

        let mut broker = welcomed(ONE);

        let _ = broker.handle(Event::Reclaim {
            now: at(10),
            guest: id(ONE),
            desired: 512 * MIB,
        });

        assert_eq!(
            message(
                &mut broker,
                at(11),
                ONE,
                GuestMessage::ReclaimNak {
                    id: 2,
                    reason: "not mine".to_owned(),
                }
            ),
            vec![closed(ONE)]
        );
    }

    #[test]
    fn a_disconnect_buries_everything_the_connection_carried() {
        let mut broker = welcomed(ONE);

        message(&mut broker, at(10), ONE, ask(9, BOOT, 3072 * MIB));
        let _ = broker.handle(Event::Reclaim {
            now: at(11),
            guest: id(ONE),
            desired: 512 * MIB,
        });

        assert_eq!(
            broker.handle(Event::Disconnected {
                now: at(12),
                peer: vsock(ONE),
            }),
            vec![]
        );

        assert!(!broker.is_negotiated(&vsock(ONE)));
        assert!(broker.negotiated(id(ONE)).is_none());

        assert_eq!(
            broker.guest(id(ONE)).and_then(Guest::requested),
            Some(3072 * MIB)
        );
    }

    #[test]
    fn a_reconnect_starts_the_id_spaces_again() {
        let mut broker = welcomed(ONE);

        message(&mut broker, at(10), ONE, ask(9, BOOT, 2560 * MIB));
        let _ = broker.handle(Event::Reclaim {
            now: at(11),
            guest: id(ONE),
            desired: 512 * MIB,
        });
        let _ = broker.handle(Event::Disconnected {
            now: at(12),
            peer: vsock(ONE),
        });
        sizes(&mut broker, at(13), ONE, 2560 * MIB);

        greet_at(&mut broker, at(20), ONE, 2560 * MIB, 512 * MIB);

        assert_eq!(
            message(&mut broker, at(21), ONE, ask(1, 2560 * MIB, 3072 * MIB)),
            vec![grant(ONE, 1, 3072 * MIB), requested(ONE, 3072 * MIB)]
        );

        assert_eq!(
            broker.handle(Event::Reclaim {
                now: at(22),
                guest: id(ONE),
                desired: 512 * MIB,
            }),
            vec![Action::Send {
                peer: vsock(ONE),
                message: HostMessage::ReclaimRequest {
                    id: 1,
                    desired: 512 * MIB,
                    deadline_ms: Duration::from_secs(30),
                },
            }]
        );
    }

    #[test]
    fn a_connection_that_arrives_over_a_live_one_replaces_it() {
        let mut broker = welcomed(ONE);

        assert_eq!(connect(&mut broker, at(10), ONE), vec![]);
        assert!(!broker.is_negotiated(&vsock(ONE)));

        assert_eq!(
            message(&mut broker, at(11), ONE, hello()),
            vec![welcome(ONE, CEILING, 0)]
        );
    }

    #[test]
    fn a_tight_host_serves_the_guest_that_asked_first() {
        let mut broker = sized();

        greet(&mut broker, at(0), ONE);
        greet(&mut broker, at(0), TWO);

        let _ = broker.handle(available(at(1), FLOOR + GIB));

        assert_eq!(
            message(&mut broker, at(10), ONE, ask(1, BOOT, 3072 * MIB)),
            vec![grant(ONE, 1, 3072 * MIB), requested(ONE, 3072 * MIB)]
        );

        assert_eq!(
            message(&mut broker, at(11), TWO, ask(1, BOOT, 3072 * MIB)),
            vec![retry(
                TWO,
                1,
                DenyReason::HostPressure,
                Duration::from_secs(5),
            )]
        );

        sizes(&mut broker, at(12), ONE, 3072 * MIB);
        let _ = broker.handle(available(at(13), FLOOR + GIB));

        assert_eq!(
            message(&mut broker, at(14), TWO, ask(2, BOOT, 2560 * MIB)),
            vec![grant(TWO, 2, 2560 * MIB), requested(TWO, 2560 * MIB)]
        );
    }

    fn holding(one: u64, two: u64) -> Broker {
        let mut broker = sized_with(revoking());

        greet(&mut broker, at(0), ONE);
        greet(&mut broker, at(0), TWO);

        sizes(&mut broker, at(1), ONE, one);
        sizes(&mut broker, at(1), TWO, two);

        broker
    }

    fn reclaimed(cid: u32, id: u64, desired: u64) -> Action {
        Action::Send {
            peer: vsock(cid),
            message: HostMessage::ReclaimRequest {
                id,
                desired,
                deadline_ms: Duration::from_secs(30),
            },
        }
    }

    #[test]
    fn a_pressured_host_takes_memory_back_from_the_guest_holding_the_most() {
        let mut broker = holding(2560 * MIB, 3072 * MIB);

        let _ = broker.handle(pressured(at(2), HOST, GIB, 25.0));

        assert_eq!(
            tick(&mut broker, at(3)),
            vec![reclaimed(TWO, 1, REVOKE_STEP)]
        );
    }

    #[test]
    fn a_host_exactly_on_its_threshold_takes_memory_back() {
        let mut broker = holding(2560 * MIB, BOOT);

        let _ = broker.handle(pressured(at(2), HOST, GIB, 20.0));

        assert_eq!(
            tick(&mut broker, at(3)),
            vec![reclaimed(ONE, 1, REVOKE_STEP)]
        );
    }

    #[test]
    fn a_revocation_asks_for_no_more_than_the_guest_grew_by() {
        let mut broker = holding(BOOT + 128 * MIB, BOOT);

        let _ = broker.handle(pressured(at(2), HOST, GIB, 25.0));

        assert_eq!(tick(&mut broker, at(3)), vec![reclaimed(ONE, 1, 128 * MIB)]);
    }

    #[test]
    fn a_revocation_leaves_a_guest_alone_while_it_is_already_being_asked() {
        let mut broker = holding(2560 * MIB, 3072 * MIB);

        let _ = broker.handle(pressured(at(2), HOST, GIB, 25.0));

        assert_eq!(
            tick(&mut broker, at(3)),
            vec![reclaimed(TWO, 1, REVOKE_STEP)]
        );
        assert_eq!(
            tick(&mut broker, at(4)),
            vec![reclaimed(ONE, 1, REVOKE_STEP)]
        );
        assert_eq!(tick(&mut broker, at(5)), vec![]);
    }

    #[test]
    fn a_host_under_its_threshold_takes_nothing_back() {
        let mut broker = holding(2560 * MIB, 3072 * MIB);

        let _ = broker.handle(pressured(at(2), HOST, GIB, 19.0));

        assert_eq!(tick(&mut broker, at(3)), vec![]);
    }

    #[test]
    fn a_host_whose_pressure_is_unread_takes_nothing_back() {
        let mut broker = holding(2560 * MIB, 3072 * MIB);

        let _ = broker.handle(available(at(2), HOST));

        assert_eq!(tick(&mut broker, at(3)), vec![]);
    }

    #[test]
    fn a_guest_still_at_the_memory_it_booted_with_is_never_revoked_from() {
        let mut broker = holding(BOOT, BOOT);

        let _ = broker.handle(pressured(at(2), HOST, GIB, 25.0));

        assert_eq!(tick(&mut broker, at(3)), vec![]);
    }

    #[test]
    fn a_host_that_was_never_asked_to_revoke_takes_nothing_back() {
        let mut broker = sized_with(policy());

        greet(&mut broker, at(0), ONE);
        sizes(&mut broker, at(1), ONE, 3072 * MIB);

        let _ = broker.handle(pressured(at(2), HOST, GIB, 90.0));

        assert_eq!(tick(&mut broker, at(3)), vec![]);
    }

    #[test]
    fn a_guest_that_refuses_a_reclaim_is_asked_no_sooner_than_one_that_says_nothing() {
        let mut broker = holding(2560 * MIB, 3072 * MIB);

        let _ = broker.handle(pressured(at(2), HOST, GIB, 25.0));

        assert_eq!(
            tick(&mut broker, at(3)),
            vec![reclaimed(TWO, 1, REVOKE_STEP)]
        );

        message(
            &mut broker,
            at(4),
            TWO,
            GuestMessage::ReclaimNak {
                id: 1,
                reason: "the workload is pinned".to_owned(),
            },
        );

        assert_eq!(
            tick(&mut broker, at(5)),
            vec![reclaimed(ONE, 1, REVOKE_STEP)],
            "the refusal stopped the host moving on to the next guest",
        );
        assert_eq!(tick(&mut broker, at(32)), vec![]);
        assert_eq!(
            tick(&mut broker, at(33)),
            vec![reclaimed(TWO, 2, REVOKE_STEP)],
            "the guest that refused was not asked again once its deadline passed",
        );
    }

    #[test]
    fn one_guest_cannot_move_another_guests_state() {
        let mut broker = sized();

        greet(&mut broker, at(0), ONE);
        greet(&mut broker, at(0), TWO);

        message(&mut broker, at(10), ONE, release(1, u64::MAX, u64::MAX));

        assert_eq!(
            broker.guest(id(TWO)).and_then(|guest| guest.size),
            Some(BOOT)
        );
        assert_eq!(broker.guest(id(TWO)).and_then(Guest::requested), None);
    }

    #[derive(Debug, Clone)]
    enum Step {
        Connect {
            cid: u32,
        },
        Hello {
            cid: u32,
            protocol: u16,
            block_size: u64,
        },
        Ask {
            cid: u32,
            id: u64,
            target: u64,
        },
        Offer {
            cid: u32,
            id: u64,
            releasable: u64,
        },
        Ack {
            cid: u32,
            id: u64,
            will_release: u64,
        },
        Nak {
            cid: u32,
            id: u64,
        },
        Report {
            cid: u32,
            total_mem: u64,
        },
        Bye {
            cid: u32,
        },
        Disconnect {
            cid: u32,
        },
        Size {
            slot: usize,
            size: u64,
        },
        Available {
            bytes: u64,
            swap_free: u64,
            psi: Option<f32>,
        },
        Reclaim {
            slot: usize,
            desired: u64,
        },
        Tick,
    }

    impl Step {
        const fn cid(&self) -> Option<u32> {
            match *self {
                Self::Connect { cid }
                | Self::Hello { cid, .. }
                | Self::Ask { cid, .. }
                | Self::Offer { cid, .. }
                | Self::Ack { cid, .. }
                | Self::Nak { cid, .. }
                | Self::Report { cid, .. }
                | Self::Bye { cid }
                | Self::Disconnect { cid } => Some(cid),
                Self::Size { .. } | Self::Available { .. } | Self::Reclaim { .. } | Self::Tick => {
                    None
                }
            }
        }

        const fn asked(&self) -> Option<u64> {
            match *self {
                Self::Ask { id, .. } | Self::Offer { id, .. } => Some(id),
                _ => None,
            }
        }

        fn event(&self, now: Timestamp) -> Event {
            let peer = self.cid().map(vsock);

            match *self {
                Self::Connect { .. } => Event::Connected {
                    now,
                    peer: peer.expect("a connection step names a peer"),
                },
                Self::Disconnect { .. } => Event::Disconnected {
                    now,
                    peer: peer.expect("a connection step names a peer"),
                },
                Self::Size { slot, size } => Event::Size {
                    hotplugged: size.saturating_sub(BOOT),
                    now,
                    guest: GuestId::from_index(slot),
                    size,
                },
                Self::Available {
                    bytes,
                    swap_free,
                    psi,
                } => Event::HostAvailable {
                    now,
                    bytes,
                    swap_free,
                    psi,
                },
                Self::Reclaim { slot, desired } => Event::Reclaim {
                    now,
                    guest: GuestId::from_index(slot),
                    desired,
                },
                Self::Tick => Event::Tick { now },
                _ => Event::Message {
                    now,
                    peer: peer.expect("a message step names a peer"),
                    message: self.message(),
                },
            }
        }

        fn message(&self) -> GuestMessage {
            match *self {
                Self::Hello {
                    protocol,
                    block_size,
                    ..
                } => GuestMessage::Hello {
                    protocol,
                    agent_version: "0.1.0".to_owned(),
                    block_size,
                    total_mem: BOOT,
                },
                Self::Ask { id, target, .. } => ask(id, BOOT, target),
                Self::Offer { id, releasable, .. } => release(id, BOOT, releasable),
                Self::Ack {
                    id, will_release, ..
                } => GuestMessage::ReclaimAck { id, will_release },
                Self::Nak { id, .. } => GuestMessage::ReclaimNak {
                    id,
                    reason: "no".to_owned(),
                },
                Self::Report { total_mem, .. } => report(total_mem, total_mem / 2),
                Self::Bye { .. } => GuestMessage::Bye {},
                _ => unreachable!("only message steps carry a message"),
            }
        }
    }

    fn byte_count() -> impl Strategy<Value = u64> {
        prop_oneof![
            1 => Just(0_u64),
            1 => Just(u64::MAX),
            8 => (0_u64..8192).prop_map(|mib| mib * MIB),
        ]
    }

    fn pressure() -> impl Strategy<Value = Option<f32>> {
        prop_oneof![
            1 => Just(None),
            4 => (0.0_f32..=100.0).prop_map(Some),
        ]
    }

    fn cid() -> impl Strategy<Value = u32> {
        prop_oneof![
            6 => Just(ONE),
            6 => Just(TWO),
            1 => Just(99_u32),
        ]
    }

    fn step() -> impl Strategy<Value = Step> {
        prop_oneof![
            3 => cid().prop_map(|cid| Step::Connect { cid }),
            4 => (cid(), prop_oneof![9 => Just(PROTOCOL_VERSION), 1 => 0_u16..4], prop_oneof![9 => Just(BLOCK), 1 => Just(0_u64)])
                .prop_map(|(cid, protocol, block_size)| Step::Hello { cid, protocol, block_size }),
            8 => (cid(), 0_u64..6, byte_count())
                .prop_map(|(cid, id, target)| Step::Ask { cid, id, target }),
            4 => (cid(), 0_u64..6, byte_count())
                .prop_map(|(cid, id, releasable)| Step::Offer { cid, id, releasable }),
            2 => (cid(), 0_u64..4, byte_count())
                .prop_map(|(cid, id, will_release)| Step::Ack { cid, id, will_release }),
            2 => (cid(), 0_u64..4).prop_map(|(cid, id)| Step::Nak { cid, id }),
            4 => (cid(), byte_count()).prop_map(|(cid, total_mem)| Step::Report { cid, total_mem }),
            1 => cid().prop_map(|cid| Step::Bye { cid }),
            2 => cid().prop_map(|cid| Step::Disconnect { cid }),
            4 => (0_usize..3, byte_count()).prop_map(|(slot, size)| Step::Size { slot, size }),
            3 => (byte_count(), byte_count(), pressure())
                .prop_map(|(bytes, swap_free, psi)| Step::Available { bytes, swap_free, psi }),
            2 => (0_usize..3, byte_count())
                .prop_map(|(slot, desired)| Step::Reclaim { slot, desired }),
            4 => Just(Step::Tick),
        ]
    }

    fn timeline() -> impl Strategy<Value = Vec<(u64, Step)>> {
        prop::collection::vec((0_u64..5_000, step()), 0..64)
    }

    #[derive(Debug, Default)]
    struct Model {
        reclaims: [u64; 3],
    }

    fn answers(actions: &[Action], peer: &PeerId) -> Vec<u64> {
        actions
            .iter()
            .filter_map(|action| match action {
                Action::Send {
                    peer: target,
                    message: HostMessage::Grant { id, .. } | HostMessage::Deny { id, .. },
                } if target == peer => Some(*id),
                _ => None,
            })
            .collect()
    }

    fn check(
        broker: &Broker,
        step: &Step,
        negotiated: bool,
        actions: &[Action],
        model: &mut Model,
    ) -> Result<(), TestCaseError> {
        for action in actions {
            match action {
                Action::Send { peer, message } => {
                    prop_assert!(
                        broker.is_negotiated(peer),
                        "sent {message:?} to {peer}, which is not a welcomed guest",
                    );

                    if let HostMessage::ReclaimRequest { id, .. } = message {
                        let slot = broker
                            .guests()
                            .lookup(peer)
                            .expect("a welcomed guest is registered")
                            .index();

                        prop_assert!(
                            *id > model.reclaims[slot],
                            "reclaim id {id} does not follow {}",
                            model.reclaims[slot],
                        );

                        model.reclaims[slot] = *id;
                    }
                }

                Action::SetRequested { guest, bytes } => {
                    let ceiling = broker
                        .guests()
                        .ceiling(*guest)
                        .expect("an actuated guest is registered");

                    prop_assert!(
                        *bytes <= ceiling.get(),
                        "actuated {bytes} over a ceiling of {ceiling}",
                    );
                }

                Action::Close { .. } => {}
            }
        }

        let Some(cid) = step.cid() else {
            prop_assert!(
                answers(actions, &vsock(ONE)).is_empty()
                    && answers(actions, &vsock(TWO)).is_empty(),
                "{step:?} was answered as though it were a request",
            );

            return Ok(());
        };

        let peer = vsock(cid);
        let answered = answers(actions, &peer);
        let closed = actions.contains(&Action::Close { peer });

        let Some(id) = step.asked().filter(|_| negotiated) else {
            prop_assert!(
                answered.is_empty(),
                "{step:?} drew {answered:?} out of the broker",
            );

            return Ok(());
        };

        if closed {
            prop_assert!(
                answered.is_empty(),
                "request {id} was both answered and hung up on",
            );
        } else {
            prop_assert_eq!(
                answered.as_slice(),
                &[id],
                "request {} was not answered exactly once",
                id,
            );
        }

        Ok(())
    }

    fn drive(policy: Policy, timeline: &[(u64, Step)]) -> Result<(), TestCaseError> {
        let mut broker = Broker::new(policy, registry());
        let mut model = Model::default();
        let mut now = Timestamp::START;

        for (gap, step) in timeline {
            now = now.saturating_add(Duration::from_millis(*gap));

            let negotiated = step
                .cid()
                .is_some_and(|cid| broker.is_negotiated(&vsock(cid)));

            if matches!(*step, Step::Connect { cid } if cid == ONE || cid == TWO) {
                let slot = step
                    .cid()
                    .and_then(|cid| broker.guests().lookup(&vsock(cid)))
                    .expect("a known guest")
                    .index();

                model.reclaims[slot] = 0;
            }

            let actions = broker.handle(step.event(now));

            check(&broker, step, negotiated, &actions, &mut model)?;
        }

        Ok(())
    }

    proptest! {
        #[test]
        fn the_broker_answers_every_request_exactly_once(timeline in timeline()) {
            drive(policy(), &timeline)?;
        }

        #[test]
        fn a_revoking_broker_answers_every_request_exactly_once(timeline in timeline()) {
            drive(revoking(), &timeline)?;
        }
    }
}
