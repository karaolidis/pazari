pub mod action;
pub mod arbiter;
pub mod effects;
pub mod event;
pub mod guest;
pub mod policy;
pub mod registry;
pub mod session;

mod handle;
mod phase;
mod world;

#[cfg(test)]
pub(crate) mod testing;

use pazari_transport::PeerId;
use pazari_types::Timestamp;

pub use crate::broker::{
    action::Action,
    arbiter::{Arbiter, Ask, Decision, Fcfs, GuestView, HostView, Request},
    effects::Effects,
    event::Event,
    guest::{Entry, Geometry, Guest, Guests, Reconcile},
    phase::{Negotiated, Phase, Reclaim, Reply, Reporting},
    policy::{Grantable, Policy, Revocation, StalledShrinks},
    registry::{GuestId, GuestSpec, Registry, RegistryError},
    session::Session,
};
use crate::broker::{phase::Step, world::World};

#[derive(Debug)]
pub struct Broker {
    world: World,
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
            world: World::new(policy, registry, arbiter),
        }
    }

    #[must_use]
    pub fn handle(&mut self, event: Event) -> Vec<Action> {
        let mut fx = Effects::default();

        self.dispatch(event, &mut fx);

        fx.drain()
    }

    fn dispatch(&mut self, event: Event, fx: &mut Effects) {
        match event {
            Event::Connected { peer } => self.on_connected(peer, fx),
            Event::Message { now, peer, message } => self.on_message(now, &peer, message, fx),
            Event::Disconnected { peer } => self.on_disconnected(&peer),
            Event::Size {
                now,
                guest,
                size,
                hotplugged,
            } => self.on_size(now, guest, size, hotplugged, fx),
            Event::HostAvailable {
                bytes,
                swap_free,
                psi,
            } => self.world.observe_host(HostView {
                available: Some(bytes),
                swap_free: Some(swap_free),
                psi,
            }),
            Event::Tick { now } => self.on_tick(now, fx),
        }
    }

    fn transition(
        &mut self,
        now: Timestamp,
        guest: GuestId,
        peer: &PeerId,
        fx: &mut Effects,
        go: impl FnOnce(&mut Step<'_>) -> Phase,
    ) -> Phase {
        let mut step = Step {
            world: &mut self.world,
            fx,
            now,
            guest,
            peer,
        };

        go(&mut step)
    }

    fn settle_phase(&mut self, guest: GuestId, phase: Phase) {
        if let Some(session) = self.world.guests_mut().session_mut(guest) {
            session.phase = phase;
        }
    }

    #[must_use]
    pub const fn policy(&self) -> &Policy {
        self.world.policy()
    }

    #[must_use]
    pub const fn guests(&self) -> &Guests {
        self.world.guests()
    }

    #[must_use]
    pub fn guest(&self, guest: GuestId) -> &Guest {
        self.world.guests().state(guest)
    }

    #[must_use]
    pub const fn host(&self) -> HostView {
        self.world.host()
    }

    #[must_use]
    pub fn is_negotiated(&self, peer: &PeerId) -> bool {
        self.world
            .guests()
            .lookup(peer)
            .and_then(|guest| self.negotiated(guest))
            .is_some()
    }

    /// Absent until the guest is connected and past the handshake.
    #[must_use]
    pub fn negotiated(&self, guest: GuestId) -> Option<&Negotiated> {
        self.world.guests().negotiated(guest)
    }

    #[must_use]
    pub fn is_stale(&self, guest: GuestId) -> bool {
        self.negotiated(guest)
            .is_some_and(|session| session.reporting() == Reporting::Stale)
    }
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU64, time::Duration};

    use pazari_proto::{DenyReason, HostMessage, PROTOCOL_VERSION};
    use pazari_types::Timestamp;

    use pazari_proto::GuestMessage;
    use proptest::prelude::*;

    use super::*;
    use crate::broker::testing::*;

    #[test]
    fn a_hello_is_answered_with_the_ceiling_and_the_cadence() {
        let mut broker = sized();

        assert_eq!(connect(&mut broker, ONE), vec![]);
        assert_eq!(
            message(&mut broker, at(0), ONE, hello()),
            vec![Action::Send {
                peer: vsock(ONE),
                message: HostMessage::Welcome {
                    protocol: PROTOCOL_VERSION,
                    ceiling: CEILING,
                    status_interval: policy().status_interval.get(),
                    hotplugged: 0,
                },
            }]
        );
        assert!(broker.is_negotiated(&vsock(ONE)));
    }

    #[test]
    fn a_guest_speaking_another_protocol_is_shown_the_door() {
        let mut broker = sized();

        connect(&mut broker, ONE);

        let wrong = GuestMessage::Hello {
            protocol: PROTOCOL_VERSION + 1,
            agent_version: "0.1.0".to_owned(),
            block_size: bytes(BLOCK),
            total_mem: BOOT,
        };

        assert_eq!(message(&mut broker, at(0), ONE, wrong), vec![closed(ONE)]);
        assert!(!broker.is_negotiated(&vsock(ONE)));
    }

    #[test]
    fn a_guest_declaring_an_impossible_geometry_is_shown_the_door() {
        let mut broker = sized();

        connect(&mut broker, ONE);

        let wrong = GuestMessage::Hello {
            protocol: PROTOCOL_VERSION,
            agent_version: "0.1.0".to_owned(),
            block_size: bytes(BLOCK),
            total_mem: 0,
        };

        assert_eq!(message(&mut broker, at(0), ONE, wrong), vec![closed(ONE)]);
    }

    #[test]
    fn a_guest_speaking_before_it_says_hello_is_shown_the_door() {
        let mut broker = sized();

        connect(&mut broker, ONE);

        assert_eq!(
            message(&mut broker, at(0), ONE, ask(1, BOOT, 3072 * MIB)),
            vec![closed(ONE)]
        );
        assert!(!broker.is_negotiated(&vsock(ONE)));
    }

    #[test]
    fn a_peer_no_guest_answers_to_is_shown_the_door() {
        let mut broker = sized();

        assert_eq!(connect(&mut broker, 99), vec![closed(99)]);
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
        assert_eq!(broker.guest(id(ONE)).requested(), Some(3072 * MIB));
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
        assert_eq!(broker.guest(id(ONE)).requested(), None);
    }

    #[test]
    fn a_grow_under_a_ceiling_off_the_block_grid_stops_at_the_last_whole_block() {
        let mut broker = ceilinged(CEILING - MIB);

        assert_eq!(
            message(&mut broker, at(10), ONE, ask(1, BOOT, 8192 * MIB)),
            vec![
                grant(ONE, 1, CEILING - BLOCK),
                requested(ONE, CEILING - BLOCK)
            ]
        );
    }

    #[test]
    fn a_guest_less_than_a_block_under_its_ceiling_is_at_its_ceiling() {
        let mut broker = ceilinged(CEILING - MIB);

        sizes(&mut broker, at(5), ONE, CEILING - BLOCK);

        assert_eq!(
            message(
                &mut broker,
                at(10),
                ONE,
                ask(1, CEILING - BLOCK, 8192 * MIB)
            ),
            vec![deny(ONE, 1, DenyReason::Ceiling)]
        );
    }

    #[test]
    fn a_guest_asking_for_no_more_than_it_has_is_granted_it_without_a_resize() {
        let mut broker = welcomed(ONE);

        assert_eq!(
            message(&mut broker, at(10), ONE, ask(1, BOOT, BOOT - GIB)),
            vec![grant(ONE, 1, BOOT - GIB)]
        );
        assert_eq!(broker.guest(id(ONE)).requested(), None);
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
    fn a_grow_into_a_shrink_in_flight_stops_the_shrink_at_what_the_guest_asked_for() {
        let mut broker = plugged(ONE);

        message(&mut broker, at(10), ONE, release(1, BOOT + GIB, 512 * MIB));

        assert_eq!(
            message(
                &mut broker,
                at(11),
                ONE,
                ask(2, BOOT + GIB, BOOT + 768 * MIB)
            ),
            vec![
                grant(ONE, 2, BOOT + 768 * MIB),
                requested(ONE, BOOT + 768 * MIB)
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

        message(&mut broker, at(10), ONE, ask(1, BOOT - RESERVED, BOOT));
        sizes(&mut broker, at(11), ONE, BOOT + 256 * MIB);

        message(
            &mut broker,
            at(12),
            ONE,
            GuestMessage::Status(status(BOOT + 64 * MIB, 512 * MIB)),
        );

        assert_eq!(broker.guest(id(ONE)).offset(), Some(192 * MIB));

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

        message(&mut broker, at(10), ONE, ask(1, BOOT - RESERVED, BOOT));

        message(
            &mut broker,
            at(11),
            ONE,
            GuestMessage::Status(status(BOOT - RESERVED, 512 * MIB)),
        );

        assert_eq!(broker.guest(id(ONE)).offset(), Some(RESERVED));
    }

    #[test]
    fn a_held_guest_is_answered_in_the_terms_it_said_hello_in() {
        let mut broker = held(BOOT - RESERVED);

        sizes(&mut broker, at(5), ONE, BOOT);

        assert_eq!(broker.guest(id(ONE)).offset(), Some(RESERVED));

        assert_eq!(
            message(&mut broker, at(10), ONE, ask(1, BOOT - RESERVED, BOOT)),
            vec![
                grant(ONE, 1, BOOT + 120 * MIB),
                requested(ONE, BOOT + 256 * MIB)
            ]
        );
    }

    #[test]
    fn a_guest_whose_offset_is_unknown_is_answered_in_the_device_s_terms() {
        let mut broker = held(BOOT + GIB);

        sizes(&mut broker, at(5), ONE, BOOT);

        assert_eq!(broker.guest(id(ONE)).offset(), None);

        assert_eq!(
            message(&mut broker, at(10), ONE, ask(1, BOOT, 3072 * MIB)),
            vec![grant(ONE, 1, 3072 * MIB), requested(ONE, 3072 * MIB)]
        );
    }

    #[test]
    fn a_hello_is_held_until_the_guest_device_reports_a_size() {
        let mut broker = held(BOOT);

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
        let mut broker = held(BOOT);

        sizes(&mut broker, at(60), ONE, BOOT);
        tick(&mut broker, at(61));

        assert!(
            !broker.is_stale(id(ONE)),
            "a guest was stale before it had a chance to say anything",
        );
    }

    #[test]
    fn a_guest_held_at_hello_negotiates_once_the_welcome_lands() {
        let mut broker = held(BOOT);

        sizes(&mut broker, at(11), ONE, BOOT);

        assert_eq!(
            message(&mut broker, at(12), ONE, ask(1, BOOT, 3072 * MIB)),
            vec![grant(ONE, 1, 3072 * MIB), requested(ONE, 3072 * MIB)]
        );
    }

    #[test]
    fn a_guest_that_speaks_while_its_hello_is_held_is_hung_up_on() {
        let mut broker = held(BOOT);

        assert_eq!(
            message(&mut broker, at(10), ONE, ask(1, BOOT, 3072 * MIB)),
            vec![closed(ONE)]
        );
        assert_eq!(broker.guest(id(ONE)).requested(), None);
    }

    #[test]
    fn a_guest_that_says_hello_twice_while_it_is_held_is_hung_up_on() {
        let mut broker = held(BOOT);

        assert_eq!(
            message(&mut broker, at(10), ONE, hello()),
            vec![closed(ONE)]
        );
    }

    #[test]
    fn a_grant_that_would_take_the_host_under_its_floor_is_denied() {
        let mut broker = welcomed(ONE);

        let _ = broker.handle(available(FLOOR + 512 * MIB));

        assert_eq!(
            message(&mut broker, at(10), ONE, ask(1, BOOT, 3072 * MIB)),
            vec![retry(
                ONE,
                1,
                DenyReason::HostPressure,
                policy().grant_min_interval.get(),
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
    fn an_offer_off_the_block_grid_releases_only_whole_blocks() {
        let mut broker = plugged(ONE);

        assert_eq!(
            message(
                &mut broker,
                at(10),
                ONE,
                release(1, BOOT + GIB, 512 * MIB + MIB)
            ),
            vec![
                grant(ONE, 1, BOOT + 512 * MIB),
                requested(ONE, BOOT + 512 * MIB),
            ]
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
        assert_eq!(broker.guest(id(ONE)).requested(), Some(3072 * MIB));

        assert_eq!(
            sizes(&mut broker, at(12), ONE, 3072 * MIB),
            vec![geometry(ONE, GIB)]
        );
        assert_eq!(broker.guest(id(ONE)).requested(), None);
        assert_eq!(broker.guest(id(ONE)).size(), Some(3072 * MIB));
    }

    #[test]
    fn a_grow_that_stalls_settles_for_what_arrived() {
        let mut broker = welcomed(ONE);

        message(&mut broker, at(10), ONE, ask(1, BOOT, 3072 * MIB));
        sizes(&mut broker, at(11), ONE, 2560 * MIB);

        assert_eq!(tick(&mut broker, at(39)), vec![]);
        assert_eq!(broker.guest(id(ONE)).requested(), Some(3072 * MIB));

        assert_eq!(tick(&mut broker, at(40)), vec![]);
        assert_eq!(broker.guest(id(ONE)).requested(), None);
        assert_eq!(broker.guest(id(ONE)).size(), Some(2560 * MIB));
    }

    #[test]
    fn a_shrink_that_stalls_is_taken_back_to_what_the_guest_has() {
        let mut broker = plugged(ONE);

        message(&mut broker, at(10), ONE, release(1, BOOT + GIB, 512 * MIB));

        assert_eq!(tick(&mut broker, at(39)), vec![]);
        assert_eq!(tick(&mut broker, at(40)), vec![requested(ONE, BOOT + GIB)]);
        assert_eq!(broker.guest(id(ONE)).requested(), None);
        assert_eq!(tick(&mut broker, at(41)), vec![]);
    }

    #[test]
    fn a_shrink_that_stalls_is_left_pending_where_policy_says_so() {
        let policy = Policy {
            stalled_shrinks: StalledShrinks::Leave,
            ..policy()
        };

        let mut broker = sized_with(policy);

        greet(&mut broker, at(0), ONE);
        sizes(&mut broker, at(1), ONE, BOOT + GIB);

        message(&mut broker, at(10), ONE, release(1, BOOT + GIB, 512 * MIB));

        assert_eq!(tick(&mut broker, at(40)), vec![]);
        assert_eq!(broker.guest(id(ONE)).requested(), None);
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
            broker.negotiated(id(ONE)).and_then(Negotiated::status),
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
            reclaim(&mut broker, at(10), ONE, 512 * MIB),
            vec![reclaimed(ONE, 1, 512 * MIB)]
        );

        assert_eq!(reclaim(&mut broker, at(11), ONE, 512 * MIB), vec![]);

        message(&mut broker, at(12), ONE, nak(1));

        assert_eq!(
            reclaim(&mut broker, at(13), ONE, 256 * MIB),
            vec![],
            "a refusal was taken as leave to ask again at once",
        );

        tick(&mut broker, at(40));

        assert_eq!(
            reclaim(&mut broker, at(41), ONE, 256 * MIB),
            vec![reclaimed(ONE, 2, 256 * MIB)]
        );
    }

    #[test]
    fn a_reclaim_the_guest_takes_on_is_settled_once_the_memory_is_offered() {
        let mut broker = plugged(ONE);

        reclaim(&mut broker, at(10), ONE, 512 * MIB);

        assert_eq!(message(&mut broker, at(11), ONE, ack(1, 256 * MIB)), vec![]);
        assert_eq!(
            broker
                .negotiated(id(ONE))
                .and_then(Negotiated::reclaim)
                .map(|reclaim| reclaim.reply),
            Some(Reply::Committed(256 * MIB))
        );

        assert_eq!(
            message(&mut broker, at(12), ONE, release(1, BOOT + GIB, 256 * MIB)),
            vec![
                grant(ONE, 1, BOOT + 768 * MIB),
                requested(ONE, BOOT + 768 * MIB),
            ]
        );
        assert_eq!(
            broker.negotiated(id(ONE)).and_then(Negotiated::reclaim),
            None,
            "the reclaim was left outstanding once the guest kept its word",
        );
    }

    #[test]
    fn a_pressured_host_waits_for_a_promised_shrink_before_asking_again() {
        let mut broker = holding(BOOT + GIB, BOOT);

        let _ = broker.handle(pressured(HOST, GIB, 25.0));

        assert_eq!(
            tick(&mut broker, at(2)),
            vec![reclaimed(ONE, 1, REVOKE_STEP)]
        );

        message(&mut broker, at(3), ONE, ack(1, REVOKE_STEP));
        message(&mut broker, at(4), ONE, release(1, BOOT + GIB, REVOKE_STEP));

        assert_eq!(
            tick(&mut broker, at(5)),
            vec![],
            "the guest was asked again while it was still shrinking",
        );

        sizes(&mut broker, at(6), ONE, BOOT + GIB - REVOKE_STEP);

        assert_eq!(
            tick(&mut broker, at(7)),
            vec![reclaimed(ONE, 2, REVOKE_STEP)]
        );
    }

    #[test]
    fn a_release_short_of_what_the_guest_promised_leaves_the_reclaim_outstanding() {
        let mut broker = plugged(ONE);

        reclaim(&mut broker, at(10), ONE, 512 * MIB);
        message(&mut broker, at(11), ONE, ack(1, 512 * MIB));
        message(&mut broker, at(12), ONE, release(1, BOOT + GIB, 256 * MIB));

        assert_eq!(
            broker
                .negotiated(id(ONE))
                .and_then(Negotiated::reclaim)
                .map(|reclaim| reclaim.reply),
            Some(Reply::Committed(512 * MIB))
        );
    }

    #[test]
    fn a_second_answer_to_one_reclaim_is_shown_the_door() {
        let mut broker = welcomed(ONE);

        reclaim(&mut broker, at(10), ONE, 512 * MIB);
        message(&mut broker, at(11), ONE, nak(1));

        assert_eq!(
            message(&mut broker, at(12), ONE, ack(1, 256 * MIB)),
            vec![closed(ONE)]
        );

        let mut broker = welcomed(ONE);

        reclaim(&mut broker, at(10), ONE, 512 * MIB);
        message(&mut broker, at(11), ONE, ack(1, 256 * MIB));

        assert_eq!(
            message(&mut broker, at(12), ONE, ack(1, 256 * MIB)),
            vec![closed(ONE)]
        );
    }

    #[test]
    fn a_reclaim_that_runs_out_of_time_is_given_up_on() {
        let mut broker = welcomed(ONE);

        reclaim(&mut broker, at(10), ONE, 512 * MIB);

        assert_eq!(tick(&mut broker, at(39)), vec![]);
        assert!(
            broker
                .negotiated(id(ONE))
                .and_then(Negotiated::reclaim)
                .is_some()
        );

        assert_eq!(tick(&mut broker, at(40)), vec![]);
        assert!(
            broker
                .negotiated(id(ONE))
                .and_then(Negotiated::reclaim)
                .is_none()
        );
    }

    #[test]
    fn an_answer_to_a_reclaim_the_host_never_asked_for_is_shown_the_door() {
        let mut broker = welcomed(ONE);

        assert_eq!(
            message(&mut broker, at(10), ONE, ack(7, 256 * MIB)),
            vec![closed(ONE)]
        );

        let mut broker = welcomed(ONE);

        reclaim(&mut broker, at(10), ONE, 512 * MIB);

        assert_eq!(message(&mut broker, at(11), ONE, nak(2)), vec![closed(ONE)]);
    }

    #[test]
    fn a_disconnect_buries_everything_the_connection_carried() {
        let mut broker = welcomed(ONE);

        message(&mut broker, at(10), ONE, ask(9, BOOT, 3072 * MIB));
        reclaim(&mut broker, at(11), ONE, 512 * MIB);

        assert_eq!(disconnect(&mut broker, ONE), vec![]);

        assert!(!broker.is_negotiated(&vsock(ONE)));
        assert!(broker.negotiated(id(ONE)).is_none());

        assert_eq!(broker.guest(id(ONE)).requested(), Some(3072 * MIB));
    }

    #[test]
    fn a_reconnect_starts_the_id_spaces_again() {
        let mut broker = welcomed(ONE);

        message(&mut broker, at(10), ONE, ask(9, BOOT, 2560 * MIB));
        reclaim(&mut broker, at(11), ONE, 512 * MIB);
        disconnect(&mut broker, ONE);
        sizes(&mut broker, at(13), ONE, 2560 * MIB);

        greet_at(&mut broker, at(20), ONE, 2560 * MIB, 512 * MIB);

        assert_eq!(
            message(&mut broker, at(21), ONE, ask(1, 2560 * MIB, 3072 * MIB)),
            vec![grant(ONE, 1, 3072 * MIB), requested(ONE, 3072 * MIB)]
        );

        assert_eq!(
            reclaim(&mut broker, at(22), ONE, 512 * MIB),
            vec![reclaimed(ONE, 1, 512 * MIB)]
        );
    }

    #[test]
    fn a_connection_that_arrives_over_a_live_one_replaces_it() {
        let mut broker = welcomed(ONE);

        assert_eq!(connect(&mut broker, ONE), vec![]);
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

        let _ = broker.handle(available(FLOOR + GIB));

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
                policy().grant_min_interval.get(),
            )]
        );

        sizes(&mut broker, at(12), ONE, 3072 * MIB);
        let _ = broker.handle(available(FLOOR + GIB));

        assert_eq!(
            message(&mut broker, at(14), TWO, ask(2, BOOT, 2560 * MIB)),
            vec![grant(TWO, 2, 2560 * MIB), requested(TWO, 2560 * MIB)]
        );
    }

    #[test]
    fn a_pressured_host_takes_memory_back_from_the_guest_holding_the_most() {
        let mut broker = holding(2560 * MIB, 3072 * MIB);

        let _ = broker.handle(pressured(HOST, GIB, 25.0));

        assert_eq!(
            tick(&mut broker, at(3)),
            vec![reclaimed(TWO, 1, REVOKE_STEP)]
        );
    }

    #[test]
    fn a_host_exactly_on_its_threshold_takes_memory_back() {
        let mut broker = holding(2560 * MIB, BOOT);

        let _ = broker.handle(pressured(HOST, GIB, 20.0));

        assert_eq!(
            tick(&mut broker, at(3)),
            vec![reclaimed(ONE, 1, REVOKE_STEP)]
        );
    }

    #[test]
    fn a_revocation_asks_for_no_more_than_the_guest_grew_by() {
        let mut broker = holding(BOOT + 128 * MIB, BOOT);

        let _ = broker.handle(pressured(HOST, GIB, 25.0));

        assert_eq!(tick(&mut broker, at(3)), vec![reclaimed(ONE, 1, 128 * MIB)]);
    }

    #[test]
    fn a_revocation_leaves_a_guest_alone_while_it_is_already_being_asked() {
        let mut broker = holding(2560 * MIB, 3072 * MIB);

        let _ = broker.handle(pressured(HOST, GIB, 25.0));

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

        let _ = broker.handle(pressured(HOST, GIB, 19.0));

        assert_eq!(tick(&mut broker, at(3)), vec![]);
    }

    #[test]
    fn a_host_whose_pressure_is_unread_takes_nothing_back() {
        let mut broker = holding(2560 * MIB, 3072 * MIB);

        let _ = broker.handle(available(HOST));

        assert_eq!(tick(&mut broker, at(3)), vec![]);
    }

    #[test]
    fn a_guest_still_at_the_memory_it_booted_with_is_never_revoked_from() {
        let mut broker = holding(BOOT, BOOT);

        let _ = broker.handle(pressured(HOST, GIB, 25.0));

        assert_eq!(tick(&mut broker, at(3)), vec![]);
    }

    #[test]
    fn a_host_that_was_never_asked_to_revoke_takes_nothing_back() {
        let mut broker = sized_with(policy());

        greet(&mut broker, at(0), ONE);
        sizes(&mut broker, at(1), ONE, 3072 * MIB);

        let _ = broker.handle(pressured(HOST, GIB, 90.0));

        assert_eq!(tick(&mut broker, at(3)), vec![]);
    }

    #[test]
    fn a_guest_that_refuses_a_reclaim_is_asked_no_sooner_than_one_that_says_nothing() {
        let mut broker = holding(2560 * MIB, 3072 * MIB);

        let _ = broker.handle(pressured(HOST, GIB, 25.0));

        assert_eq!(
            tick(&mut broker, at(3)),
            vec![reclaimed(TWO, 1, REVOKE_STEP)]
        );

        message(&mut broker, at(4), TWO, nak(1));

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

        assert_eq!(broker.guest(id(TWO)).size(), Some(BOOT));
        assert_eq!(broker.guest(id(TWO)).requested(), None);
    }

    #[derive(Debug, Clone)]
    enum Step {
        Connect {
            cid: u32,
        },
        Hello {
            cid: u32,
            protocol: u16,
            block_size: NonZeroU64,
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
            psi: Option<f64>,
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

        fn apply(&self, broker: &mut Broker, now: Timestamp) -> Vec<Action> {
            let peer = self.cid().map(vsock);

            let event = match *self {
                Self::Connect { .. } => Event::Connected {
                    peer: peer.expect("a connection step names a peer"),
                },
                Self::Disconnect { .. } => Event::Disconnected {
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
                    bytes,
                    swap_free,
                    psi,
                },
                Self::Reclaim { slot, desired } => {
                    let mut fx = Effects::default();

                    broker.on_reclaim(now, GuestId::from_index(slot), desired, &mut fx);

                    return fx.drain();
                }
                Self::Tick => Event::Tick { now },
                _ => Event::Message {
                    now,
                    peer: peer.expect("a message step names a peer"),
                    message: self.message(),
                },
            };

            broker.handle(event)
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
                } => GuestMessage::ReclaimAck {
                    id: ReclaimId(id),
                    will_release,
                },
                Self::Nak { id, .. } => GuestMessage::ReclaimNak {
                    id: ReclaimId(id),
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

    fn pressure() -> impl Strategy<Value = Option<f64>> {
        prop_oneof![
            1 => Just(None),
            4 => (0.0_f64..=100.0).prop_map(Some),
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
            4 => (cid(), prop_oneof![9 => Just(PROTOCOL_VERSION), 1 => 0_u16..4], prop_oneof![9 => Just(bytes(BLOCK)), 1 => Just(bytes(4096))])
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
            4 => (0_usize..2, byte_count()).prop_map(|(slot, size)| Step::Size { slot, size }),
            3 => (byte_count(), byte_count(), pressure())
                .prop_map(|(bytes, swap_free, psi)| Step::Available { bytes, swap_free, psi }),
            2 => (0_usize..2, byte_count())
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
                } if target == peer => Some(id.0),
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
                            id.0 > model.reclaims[slot],
                            "reclaim id {id} does not follow {}",
                            model.reclaims[slot],
                        );

                        model.reclaims[slot] = id.0;
                    }
                }

                Action::SetRequested { guest, bytes } => {
                    let ceiling = broker.guests().ceiling(*guest);

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

            let actions = step.apply(&mut broker, now);

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
