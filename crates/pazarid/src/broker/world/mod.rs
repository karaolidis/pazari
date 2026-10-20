use std::num::NonZeroU64;

use pazari_proto::{DenyReason, HostMessage, RequestId};
use pazari_transport::PeerId;
use pazari_types::{Deadline, Timestamp};
use tracing::debug;

use crate::broker::{
    action::Action,
    arbiter::{Arbiter, Ask, Decision, HostView, Request},
    effects::Effects,
    guest::{Guest, Guests, Reconcile},
    policy::Policy,
    registry::{GuestId, Registry},
    session::Session,
};

#[derive(Debug)]
pub struct World {
    policy: Policy,
    guests: Guests,
    arbiter: Box<dyn Arbiter>,
    host: HostView,
}

impl World {
    pub fn new(policy: Policy, registry: Registry, arbiter: impl Arbiter + 'static) -> Self {
        Self {
            policy,
            guests: Guests::new(registry),
            arbiter: Box::new(arbiter),
            host: HostView::default(),
        }
    }

    pub const fn policy(&self) -> &Policy {
        &self.policy
    }

    pub const fn guests(&self) -> &Guests {
        &self.guests
    }

    pub const fn guests_mut(&mut self) -> &mut Guests {
        &mut self.guests
    }

    pub const fn host(&self) -> HostView {
        self.host
    }

    pub const fn observe_host(&mut self, host: HostView) {
        self.host = host;
    }

    #[must_use]
    pub fn name(&self, guest: GuestId) -> &str {
        self.guests.name(guest)
    }

    #[must_use]
    pub fn ceiling(&self, guest: GuestId) -> Option<NonZeroU64> {
        self.guests.ceiling(guest)
    }

    #[must_use]
    pub fn size(&self, guest: GuestId) -> Option<u64> {
        self.guests.state(guest)?.size()
    }

    #[must_use]
    pub fn hotplugged(&self, guest: GuestId) -> Option<u64> {
        self.guests.state(guest).and_then(Guest::hotplugged)
    }

    #[must_use]
    pub fn offset(&self, guest: GuestId) -> u64 {
        self.guests
            .state(guest)
            .and_then(Guest::offset)
            .unwrap_or(0)
    }

    pub fn answer(&self, peer: &PeerId, id: RequestId, decision: Decision, fx: &mut Effects) {
        let guest = self.guests.lookup(peer).map(|guest| self.name(guest));

        let message = match decision {
            Decision::Grant { target } => {
                debug!(?guest, %id, target, "granting memory");

                HostMessage::Grant { id, target }
            }
            Decision::Deny {
                reason,
                retry_after,
            } => {
                debug!(
                    ?guest,
                    %id,
                    ?reason,
                    ?retry_after,
                    available = ?self.host.available,
                    floor = self.policy.host_floor,
                    "denying",
                );

                HostMessage::Deny {
                    id,
                    reason,
                    retry_after,
                }
            }
        };

        fx.send(peer, message);
    }

    pub fn close(&mut self, guest: GuestId, peer: &PeerId, fx: &mut Effects) {
        if let Some(state) = self.guests.state_mut(guest) {
            state.session = None;
        }

        fx.close(peer);
    }

    pub fn open_session(&mut self, guest: GuestId, peer: PeerId) -> bool {
        self.guests
            .state_mut(guest)
            .is_some_and(|state| state.session.replace(Session::new(peer)).is_some())
    }

    pub fn drop_session(&mut self, guest: GuestId) -> bool {
        self.guests
            .state_mut(guest)
            .is_some_and(|state| state.session.take().is_some())
    }

    pub const fn unsized_deny(&self) -> Decision {
        Decision::Deny {
            reason: DenyReason::Policy,
            retry_after: Some(self.policy.grant_min_interval.get()),
        }
    }

    pub fn arbitrate(&mut self, now: Timestamp, guest: GuestId, ask: Ask) -> Decision {
        let (asking, others) = self.guests.views(guest);

        let Some(asking) = asking else {
            return Decision::UNKNOWN;
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

    pub fn revoke(&mut self) -> Option<(GuestId, u64)> {
        let revocation = self.policy.revocation?;

        let candidates: Vec<_> = self
            .guests
            .iter()
            .filter(|(_, entry)| entry.state.geometry.is_some() && !entry.state.asked())
            .map(|(id, entry)| entry.view(id))
            .collect();

        self.arbiter.revoke(&candidates, revocation)
    }

    pub fn tell_geometry(&self, guest: GuestId, peer: &PeerId, fx: &mut Effects) {
        let Some(hotplugged) = self.hotplugged(guest) else {
            return;
        };

        fx.send(peer, HostMessage::Geometry { hotplugged });
    }

    pub fn observe_total_mem(&mut self, guest: GuestId, total_mem: u64) {
        if let Some(state) = self.guests.state_mut(guest) {
            state.observe(total_mem);
        }
    }

    pub fn note_granted(&mut self, guest: GuestId, now: Timestamp) {
        if let Some(state) = self.guests.state_mut(guest) {
            state.granted = Some(now);
        }
    }

    pub fn actuate(&mut self, now: Timestamp, guest: GuestId, target: u64, fx: &mut Effects) {
        let deadline = Deadline::after(now, self.policy.reconcile_deadline.get());

        let Some(state) = self.guests.state_mut(guest) else {
            return;
        };

        let Some(size) = state.size() else {
            return;
        };

        if target == size {
            if state.reconcile.take().is_some() {
                fx.emit(Action::SetRequested {
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

        fx.emit(Action::SetRequested {
            guest,
            bytes: target,
        });
    }

    pub fn settle(&mut self, guest: GuestId, size: u64, hotplugged: u64) {
        let name = self.guests.name(guest).to_owned();

        let Some(state) = self.guests.state_mut(guest) else {
            return;
        };

        state.settle(size, hotplugged);

        let Some(reconcile) = state.reconcile else {
            return;
        };

        if !reconcile.converged(size) {
            return;
        }

        state.reconcile = None;

        debug!(
            guest = name,
            size,
            target = reconcile.target,
            "actuation completed"
        );
    }
}
