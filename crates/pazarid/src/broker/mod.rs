pub mod action;
pub mod arbiter;
pub mod event;
pub mod guest;
pub mod policy;
pub mod registry;
pub mod session;

#[cfg(test)]
pub(crate) mod testing;

use pazari_proto::{DenyReason, GuestMessage, HostMessage, PROTOCOL_VERSION, Status};
use pazari_transport::PeerId;
use pazari_types::Timestamp;
use tracing::{debug, info, warn};

pub use crate::broker::{
    action::Action,
    arbiter::{Arbiter, Ask, Decision, Fcfs, GuestView, HostView, Request},
    event::Event,
    guest::{Guest, Reconcile},
    policy::{Policy, Revocation},
    registry::{GuestId, GuestSpec, Registry, RegistryError},
    session::{Negotiated, Phase, Reclaim, Session},
};

/// The host's negotiation state machine.
#[derive(Debug, Clone)]
pub struct Broker<A = Fcfs> {
    policy: Policy,
    registry: Registry,
    arbiter: A,
    guests: Vec<Guest>,
    host: HostView,
}

impl Broker<Fcfs> {
    #[must_use]
    pub fn new(policy: Policy, guests: Registry) -> Self {
        Self::with_arbiter(policy, guests, Fcfs)
    }
}

impl<A: Arbiter> Broker<A> {
    #[must_use]
    pub fn with_arbiter(policy: Policy, registry: Registry, arbiter: A) -> Self {
        let guests = vec![Guest::default(); registry.len()];

        Self {
            policy,
            registry,
            arbiter,
            guests,
            host: HostView::default(),
        }
    }

    #[must_use]
    pub fn handle(&mut self, event: Event) -> Vec<Action> {
        let mut out = Vec::new();

        match event {
            Event::Connected { now, peer } => self.on_connected(&mut out, now, peer),
            Event::Message { now, peer, message } => self.on_message(&mut out, now, &peer, message),
            Event::Disconnected { now, peer } => self.on_disconnected(now, &peer),
            Event::Size { now, guest, size } => self.on_size(now, guest, size),
            Event::HostAvailable {
                bytes,
                swap_free,
                psi,
                ..
            } => {
                self.host = HostView {
                    available: Some(bytes),
                    swap_free: Some(swap_free),
                    psi,
                };
            }
            Event::Reclaim {
                now,
                guest,
                desired,
            } => self.on_reclaim(&mut out, now, guest, desired),
            Event::Tick { now } => self.on_tick(&mut out, now),
        }

        out
    }

    #[must_use]
    pub const fn policy(&self) -> &Policy {
        &self.policy
    }

    #[must_use]
    pub const fn registry(&self) -> &Registry {
        &self.registry
    }

    #[must_use]
    pub fn guest(&self, guest: GuestId) -> Option<&Guest> {
        self.guests.get(guest.index())
    }

    #[must_use]
    pub const fn host(&self) -> HostView {
        self.host
    }

    #[must_use]
    pub const fn host_available(&self) -> Option<u64> {
        self.host.available
    }

    #[must_use]
    pub fn is_negotiated(&self, peer: &PeerId) -> bool {
        self.registry
            .lookup(peer)
            .and_then(|guest| self.negotiated(guest))
            .is_some()
    }

    #[must_use]
    pub fn is_stale(&self, guest: GuestId) -> bool {
        self.negotiated(guest).is_some_and(|session| session.stale)
    }

    fn on_connected(&mut self, out: &mut Vec<Action>, _now: Timestamp, peer: PeerId) {
        let Some(guest) = self.registry.lookup(&peer) else {
            warn!(%peer, "a connection from a peer no guest answers to");
            out.push(Action::Close { peer });
            return;
        };

        let Some(state) = self.guests.get_mut(guest.index()) else {
            return;
        };

        let replaced = state.session.replace(Session::new(peer.clone())).is_some();

        if replaced {
            info!(guest = self.name(guest), %peer, "the guest connected again over a connection the host still held");
        } else {
            debug!(guest = self.name(guest), %peer, "a guest connected");
        }
    }

    fn on_message(
        &mut self,
        out: &mut Vec<Action>,
        now: Timestamp,
        peer: &PeerId,
        message: GuestMessage,
    ) {
        let Some(guest) = self.registry.lookup(peer) else {
            warn!(%peer, "a message from a peer no guest answers to");
            out.push(Action::Close { peer: peer.clone() });
            return;
        };

        let phase = self
            .guests
            .get(guest.index())
            .and_then(|state| state.session.as_ref())
            .map(|session| matches!(session.phase, Phase::AwaitingHello));

        match phase {
            Some(true) => self.on_hello(out, now, guest, peer, message),
            Some(false) => self.on_negotiated(out, now, guest, peer, message),
            None => {
                warn!(guest = self.name(guest), %peer, "a message on a connection the broker never accepted");
                out.push(Action::Close { peer: peer.clone() });
            }
        }
    }

    fn on_hello(
        &mut self,
        out: &mut Vec<Action>,
        now: Timestamp,
        guest: GuestId,
        peer: &PeerId,
        message: GuestMessage,
    ) {
        let GuestMessage::Hello {
            protocol,
            agent_version,
            boot_mem,
            block_size,
            plugged,
        } = message
        else {
            warn!(guest = self.name(guest), %peer, "the guest spoke before it said hello");
            self.close(out, guest, peer);
            return;
        };

        if protocol != PROTOCOL_VERSION {
            warn!(
                guest = self.name(guest),
                %peer,
                theirs = protocol,
                ours = PROTOCOL_VERSION,
                "the guest speaks another protocol",
            );
            self.close(out, guest, peer);
            return;
        }

        if boot_mem == 0 || block_size == 0 {
            warn!(
                guest = self.name(guest),
                %peer,
                boot_mem,
                block_size,
                "the guest declared a geometry that cannot be true",
            );
            self.close(out, guest, peer);
            return;
        }

        let ceiling = self.registry.ceiling(guest);
        let status_interval_ms = self.policy.welcome_status_interval.get();

        if let Some(session) = self
            .guests
            .get_mut(guest.index())
            .and_then(|state| state.session.as_mut())
        {
            session.phase = Phase::Negotiated(Negotiated::new(boot_mem, block_size, now));
        }

        info!(
            guest = self.name(guest),
            %peer,
            agent_version,
            boot_mem,
            block_size,
            plugged,
            ceiling,
            "welcoming a guest",
        );

        out.push(Action::Send {
            peer: peer.clone(),
            message: HostMessage::Welcome {
                protocol: PROTOCOL_VERSION,
                ceiling,
                status_interval_ms,
            },
        });
    }

    fn on_negotiated(
        &mut self,
        out: &mut Vec<Action>,
        now: Timestamp,
        guest: GuestId,
        peer: &PeerId,
        message: GuestMessage,
    ) {
        match message {
            GuestMessage::Hello { .. } => {
                warn!(guest = self.name(guest), %peer, "the guest said hello twice");
                self.close(out, guest, peer);
            }

            GuestMessage::GrantRequest {
                id,
                target,
                reason,
                urgency,
                ..
            } => {
                let ceiling = self.registry.ceiling(guest);
                let target = target.min(ceiling);

                let ask = self
                    .size(guest)
                    .ok_or_else(|| Decision::Deny {
                        reason: DenyReason::Policy,
                        retry_after: Some(self.policy.grant_min_interval.get()),
                    })
                    .and_then(|size| {
                        if target > size {
                            Ok(Ask::Grow {
                                target,
                                reason,
                                urgency,
                            })
                        } else {
                            Err(Decision::Deny {
                                reason: DenyReason::Ceiling,
                                retry_after: None,
                            })
                        }
                    });

                self.on_request(out, now, guest, peer, id, ask);
            }

            GuestMessage::OfferRelease { id, releasable, .. } => {
                let ask = self
                    .size(guest)
                    .ok_or_else(|| Decision::Deny {
                        reason: DenyReason::Policy,
                        retry_after: Some(self.policy.grant_min_interval.get()),
                    })
                    .map(|size| Ask::Release {
                        target: size.saturating_sub(releasable),
                    });

                self.on_request(out, now, guest, peer, id, ask);
            }

            GuestMessage::ReclaimAck { id, will_release } => {
                self.on_reclaim_ack(out, guest, peer, id, will_release);
            }

            GuestMessage::ReclaimNak { id, reason } => {
                self.on_reclaim_nak(out, guest, peer, id, &reason);
            }

            GuestMessage::Status(status) => self.on_status(now, guest, status),

            GuestMessage::Bye {} => {
                debug!(guest = self.name(guest), %peer, "the guest said goodbye");
                self.close(out, guest, peer);
            }
        }
    }

    fn on_request(
        &mut self,
        out: &mut Vec<Action>,
        now: Timestamp,
        guest: GuestId,
        peer: &PeerId,
        id: u64,
        ask: Result<Ask, Decision>,
    ) {
        let accepted = self
            .negotiated_mut(guest)
            .is_some_and(|session| session.accept(id));

        if !accepted {
            warn!(guest = self.name(guest), %peer, id, "a request id that does not follow the last one");
            self.close(out, guest, peer);
            return;
        }

        let ask = match ask {
            Ok(ask) => ask,
            Err(decision) => {
                out.push(Self::answer(peer, id, decision));
                return;
            }
        };

        let decision = self.arbitrate(now, guest, ask);

        let Decision::Grant { target } = decision else {
            out.push(Self::answer(peer, id, decision));
            return;
        };

        let target = target.min(self.registry.ceiling(guest));

        out.push(Self::answer(peer, id, Decision::Grant { target }));

        if matches!(ask, Ask::Grow { .. })
            && let Some(state) = self.guests.get_mut(guest.index())
        {
            state.granted = Some(now);
        }

        self.actuate(out, now, guest, target);
    }

    fn arbitrate(&mut self, now: Timestamp, guest: GuestId, ask: Ask) -> Decision {
        let mut asking = None;
        let mut others = Vec::with_capacity(self.registry.len());

        for (id, spec) in self.registry.iter() {
            let Some(state) = self.guests.get(id.index()) else {
                continue;
            };

            let view = view(id, spec, state);

            if id == guest {
                asking = Some(view);
            } else {
                others.push(view);
            }
        }

        let Some(asking) = asking else {
            return Decision::Deny {
                reason: DenyReason::Policy,
                retry_after: None,
            };
        };

        let request = Request {
            now,
            guest: asking,
            ask,
            others: &others,
            host: self.host,
            policy: &self.policy,
        };

        self.arbiter.decide(&request)
    }

    fn actuate(&mut self, out: &mut Vec<Action>, now: Timestamp, guest: GuestId, target: u64) {
        let deadline = now.saturating_add(self.policy.reconcile_deadline.get());

        let Some(state) = self.guests.get_mut(guest.index()) else {
            return;
        };

        let Some(size) = state.size else {
            return;
        };

        if target == size {
            if state.reconcile.take().is_some() {
                out.push(Action::SetRequested {
                    guest,
                    bytes: target,
                });
            }

            return;
        }

        state.reconcile = Some(Reconcile {
            target,
            from: size,
            deadline,
        });

        out.push(Action::SetRequested {
            guest,
            bytes: target,
        });
    }

    fn on_reclaim(&mut self, out: &mut Vec<Action>, now: Timestamp, guest: GuestId, desired: u64) {
        let Some(session) = self
            .guests
            .get(guest.index())
            .and_then(|state| state.session.as_ref())
        else {
            warn!(
                guest = self.name(guest),
                "nothing to reclaim from: the guest is not connected"
            );
            return;
        };

        let peer = session.peer.clone();

        let Some(negotiated) = session.negotiated() else {
            warn!(
                guest = self.name(guest),
                "nothing to reclaim from: the guest has not said hello"
            );
            return;
        };

        if let Some(reclaim) = negotiated.reclaim {
            warn!(
                guest = self.name(guest),
                outstanding = reclaim.id,
                refused = reclaim.refused,
                "the guest is not to be asked for memory again yet",
            );

            return;
        }

        let deadline_ms = self.policy.reclaim_deadline.get();

        let Some(session) = self.negotiated_mut(guest) else {
            return;
        };

        let id = session.take_reclaim_id();

        session.reclaim = Some(Reclaim {
            id,
            desired,
            deadline: now.saturating_add(deadline_ms),
            committed: None,
            refused: false,
        });

        info!(
            guest = self.name(guest),
            id, desired, "asking a guest for memory back"
        );

        out.push(Action::Send {
            peer,
            message: HostMessage::ReclaimRequest {
                id,
                desired,
                deadline_ms,
            },
        });
    }

    fn on_reclaim_ack(
        &mut self,
        out: &mut Vec<Action>,
        guest: GuestId,
        peer: &PeerId,
        id: u64,
        will_release: u64,
    ) {
        if !self.answers_reclaim(guest, id) {
            warn!(guest = self.name(guest), %peer, id, "an answer to a reclaim the host never asked for");
            self.close(out, guest, peer);
            return;
        }

        if let Some(session) = self.negotiated_mut(guest)
            && let Some(reclaim) = session.reclaim.as_mut()
        {
            reclaim.committed = Some(will_release);
        }

        info!(
            guest = self.name(guest),
            id, will_release, "the guest took on a release"
        );
    }

    fn on_reclaim_nak(
        &mut self,
        out: &mut Vec<Action>,
        guest: GuestId,
        peer: &PeerId,
        id: u64,
        reason: &str,
    ) {
        if !self.answers_reclaim(guest, id) {
            warn!(guest = self.name(guest), %peer, id, "an answer to a reclaim the host never asked for");
            self.close(out, guest, peer);
            return;
        }

        if let Some(session) = self.negotiated_mut(guest)
            && let Some(reclaim) = session.reclaim.as_mut()
        {
            reclaim.refused = true;
        }

        info!(
            guest = self.name(guest),
            id, reason, "the guest refused a release"
        );
    }

    fn on_status(&mut self, now: Timestamp, guest: GuestId, status: Status) {
        let Some(session) = self.negotiated_mut(guest) else {
            return;
        };

        let returned = session.stale;

        session.stale = false;
        session.heard = now;
        session.status = Some(status);

        if returned {
            info!(
                guest = self.name(guest),
                "the guest is sending status again"
            );
        }
    }

    fn on_size(&mut self, _now: Timestamp, guest: GuestId, size: u64) {
        let Some(state) = self.guests.get_mut(guest.index()) else {
            return;
        };

        state.size = Some(size);

        let Some(reconcile) = state.reconcile else {
            return;
        };

        if !reconcile.converged(size) {
            return;
        }

        state.reconcile = None;

        debug!(
            guest = self.name(guest),
            size,
            target = reconcile.target,
            "an actuation completed"
        );
    }

    fn on_disconnected(&mut self, _now: Timestamp, peer: &PeerId) {
        let Some(guest) = self.registry.lookup(peer) else {
            return;
        };

        let Some(state) = self.guests.get_mut(guest.index()) else {
            return;
        };

        if state.session.take().is_some() {
            debug!(guest = self.name(guest), %peer, "a guest disconnected");
        }
    }

    fn on_tick(&mut self, out: &mut Vec<Action>, now: Timestamp) {
        for slot in 0..self.guests.len() {
            let guest = GuestId::from_index(slot);

            self.tick_reconcile(out, now, guest);
            self.tick_reclaim(now, guest);
            self.tick_staleness(now, guest);
        }

        self.tick_revocation(out, now);
    }

    fn tick_revocation(&mut self, out: &mut Vec<Action>, now: Timestamp) {
        let Some(revocation) = self.policy.revocation else {
            return;
        };

        let Some(psi) = self.host.psi else {
            return;
        };

        if f64::from(psi) < revocation.psi.get() {
            return;
        }

        let mut candidates = Vec::with_capacity(self.registry.len());

        for (id, spec) in self.registry.iter() {
            let Some(state) = self.guests.get(id.index()) else {
                continue;
            };

            let asked = state
                .session
                .as_ref()
                .and_then(Session::negotiated)
                .is_none_or(|session| session.reclaim.is_some());

            if state.size.is_some() && !asked {
                candidates.push(view(id, spec, state));
            }
        }

        let Some((guest, desired)) = self.arbiter.revoke(&candidates, revocation) else {
            return;
        };

        warn!(
            guest = self.name(guest),
            psi,
            threshold = revocation.psi.get(),
            "the host is under pressure; taking memory back",
        );

        self.on_reclaim(out, now, guest, desired);
    }

    fn tick_reconcile(&mut self, out: &mut Vec<Action>, now: Timestamp, guest: GuestId) {
        let revert = self.policy.shrink_revert;
        let ceiling = self.registry.ceiling(guest);

        let Some(state) = self.guests.get_mut(guest.index()) else {
            return;
        };

        let Some(reconcile) = state.reconcile else {
            return;
        };

        if now < reconcile.deadline {
            return;
        }

        state.reconcile = None;

        let Some(size) = state.size else {
            return;
        };

        if !reconcile.is_shrink() {
            info!(
                guest = self.name(guest),
                target = reconcile.target,
                size,
                "a grow did not finish in time; taking the size it reached",
            );

            return;
        }

        if revert {
            out.push(Action::SetRequested {
                guest,
                bytes: size.min(ceiling),
            });
        }

        warn!(
            guest = self.name(guest),
            target = reconcile.target,
            size,
            reverted = revert,
            "a shrink did not finish in time",
        );
    }

    fn tick_reclaim(&mut self, now: Timestamp, guest: GuestId) {
        let Some(session) = self.negotiated_mut(guest) else {
            return;
        };

        let Some(reclaim) = session.reclaim else {
            return;
        };

        if now < reclaim.deadline {
            return;
        }

        session.reclaim = None;

        if reclaim.refused {
            debug!(
                guest = self.name(guest),
                id = reclaim.id,
                "the guest may be asked for memory again",
            );

            return;
        }

        warn!(
            guest = self.name(guest),
            id = reclaim.id,
            desired = reclaim.desired,
            committed = ?reclaim.committed,
            "a reclaim ran out of time",
        );
    }

    fn tick_staleness(&mut self, now: Timestamp, guest: GuestId) {
        let window = self
            .policy
            .welcome_status_interval
            .get()
            .saturating_mul(self.policy.stale_after.get());

        let Some(session) = self.negotiated_mut(guest) else {
            return;
        };

        if session.stale {
            return;
        }

        let quiet = now.saturating_duration_since(session.heard);

        if quiet <= window {
            return;
        }

        session.stale = true;

        warn!(
            guest = self.name(guest),
            ?quiet,
            "the guest has stopped sending status"
        );
    }

    fn close(&mut self, out: &mut Vec<Action>, guest: GuestId, peer: &PeerId) {
        if let Some(state) = self.guests.get_mut(guest.index()) {
            state.session = None;
        }

        out.push(Action::Close { peer: peer.clone() });
    }

    fn answer(peer: &PeerId, id: u64, decision: Decision) -> Action {
        let message = match decision {
            Decision::Grant { target } => HostMessage::Grant { id, target },
            Decision::Deny {
                reason,
                retry_after,
            } => HostMessage::Deny {
                id,
                reason,
                retry_after_ms: retry_after,
            },
        };

        Action::Send {
            peer: peer.clone(),
            message,
        }
    }

    fn answers_reclaim(&self, guest: GuestId, id: u64) -> bool {
        self.negotiated(guest)
            .and_then(|session| session.reclaim)
            .is_some_and(|reclaim| reclaim.id == id)
    }

    fn size(&self, guest: GuestId) -> Option<u64> {
        self.guest(guest).and_then(|state| state.size)
    }

    fn negotiated(&self, guest: GuestId) -> Option<&Negotiated> {
        self.guests
            .get(guest.index())
            .and_then(|state| state.session.as_ref())
            .and_then(Session::negotiated)
    }

    fn negotiated_mut(&mut self, guest: GuestId) -> Option<&mut Negotiated> {
        self.guests
            .get_mut(guest.index())
            .and_then(|state| state.session.as_mut())
            .and_then(Session::negotiated_mut)
    }

    fn name(&self, guest: GuestId) -> &str {
        self.registry
            .spec(guest)
            .map_or("?", |spec| spec.name.as_str())
    }
}

fn view<'a>(id: GuestId, spec: &'a GuestSpec, state: &Guest) -> GuestView<'a> {
    let negotiated = state.session.as_ref().and_then(Session::negotiated);

    GuestView {
        id,
        spec,
        boot_mem: negotiated.map(|session| session.boot_mem),
        size: state.size.unwrap_or(0),
        requested: state.requested(),
        status: negotiated.and_then(|session| session.status),
        stale: negotiated.is_some_and(|session| session.stale),
        connected: negotiated.is_some(),
        granted: state.granted,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

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
            boot_mem: BOOT,
            block_size: BLOCK,
            plugged: BOOT,
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
            boot_mem: BOOT,
            block_size: 0,
            plugged: BOOT,
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

    fn unsized_guest() -> Broker {
        let mut broker = Broker::new(policy(), registry());

        let _ = broker.handle(available(at(0), HOST));
        greet(&mut broker, at(0), ONE);

        broker
    }

    #[test]
    fn a_grow_from_a_guest_whose_size_is_unknown_is_denied_once() {
        let mut broker = unsized_guest();

        assert_eq!(
            message(&mut broker, at(10), ONE, ask(1, BOOT, 3072 * MIB)),
            vec![retry(ONE, 1, DenyReason::Policy, Duration::from_secs(5))]
        );
        assert_eq!(broker.guest(id(ONE)).and_then(Guest::requested), None);
    }

    #[test]
    fn a_release_from_a_guest_whose_size_is_unknown_is_denied_once() {
        let mut broker = unsized_guest();

        assert_eq!(
            message(&mut broker, at(10), ONE, release(1, BOOT, 512 * MIB)),
            vec![retry(ONE, 1, DenyReason::Policy, Duration::from_secs(5))]
        );
        assert_eq!(broker.guest(id(ONE)).and_then(Guest::requested), None);
    }

    #[test]
    fn a_guest_negotiates_as_soon_as_its_device_reports_a_size() {
        let mut broker = unsized_guest();

        message(&mut broker, at(10), ONE, ask(1, BOOT, 3072 * MIB));
        sizes(&mut broker, at(11), ONE, BOOT);

        assert_eq!(
            message(&mut broker, at(12), ONE, ask(2, BOOT, 3072 * MIB)),
            vec![grant(ONE, 2, 3072 * MIB), requested(ONE, 3072 * MIB)]
        );

        let mut broker = unsized_guest();

        sizes(&mut broker, at(11), ONE, BOOT);

        assert_eq!(
            message(&mut broker, at(12), ONE, release(1, BOOT, 512 * MIB)),
            vec![grant(ONE, 1, 1536 * MIB), requested(ONE, 1536 * MIB)]
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
        let mut broker = welcomed(ONE);

        assert_eq!(
            message(&mut broker, at(10), ONE, release(1, BOOT, 512 * MIB)),
            vec![grant(ONE, 1, 1536 * MIB), requested(ONE, 1536 * MIB),]
        );
    }

    #[test]
    fn an_offer_of_more_than_the_guest_holds_stops_at_nothing() {
        let mut broker = welcomed(ONE);

        assert_eq!(
            message(&mut broker, at(10), ONE, release(1, BOOT, u64::MAX)),
            vec![grant(ONE, 1, 0), requested(ONE, 0)]
        );
    }

    #[test]
    fn a_release_does_not_arm_the_rate_limit() {
        let mut broker = welcomed(ONE);

        message(&mut broker, at(10), ONE, release(1, BOOT, 512 * MIB));
        sizes(&mut broker, at(11), ONE, 1536 * MIB);

        assert_eq!(
            message(&mut broker, at(12), ONE, ask(2, 1536 * MIB, 2560 * MIB)),
            vec![grant(ONE, 2, 2560 * MIB), requested(ONE, 2560 * MIB)]
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

        assert_eq!(sizes(&mut broker, at(11), ONE, 2560 * MIB), vec![]);
        assert_eq!(
            broker.guest(id(ONE)).and_then(Guest::requested),
            Some(3072 * MIB)
        );

        assert_eq!(sizes(&mut broker, at(12), ONE, 3072 * MIB), vec![]);
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
        let mut broker = welcomed(ONE);

        message(&mut broker, at(10), ONE, release(1, BOOT, 512 * MIB));

        assert_eq!(tick(&mut broker, at(39)), vec![]);
        assert_eq!(tick(&mut broker, at(40)), vec![requested(ONE, BOOT)]);
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

        message(&mut broker, at(10), ONE, release(1, BOOT, 512 * MIB));

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
            message(&mut broker, at(12), ONE, release(1, BOOT, 256 * MIB)),
            vec![grant(ONE, 1, 1792 * MIB), requested(ONE, 1792 * MIB),]
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

        greet(&mut broker, at(20), ONE);

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
            vec![welcome(ONE, CEILING)]
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
            plugged: u64,
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
                    boot_mem: BOOT,
                    block_size,
                    plugged: BOOT,
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
                Self::Report { plugged, .. } => report(plugged, plugged / 2),
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
            4 => (cid(), byte_count()).prop_map(|(cid, plugged)| Step::Report { cid, plugged }),
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
                            .registry()
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
                    let ceiling = broker.registry().ceiling(*guest);

                    prop_assert!(
                        *bytes <= ceiling,
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
                    .and_then(|cid| broker.registry().lookup(&vsock(cid)))
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
