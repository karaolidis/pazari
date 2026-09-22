use pazari_proto::{DenyReason, HostMessage};
use pazari_transport::PeerId;
use pazari_types::Timestamp;
use tracing::debug;

use crate::broker::{
    action::Action,
    arbiter::{Arbiter, Ask, Decision, HostView, Request},
    guest::{Guests, Reconcile},
    phase::Said,
    policy::Policy,
    registry::{GuestId, Registry},
};

#[derive(Debug)]
pub struct Context {
    pub policy: Policy,
    pub guests: Guests,
    pub arbiter: Box<dyn Arbiter>,
    pub host: HostView,

    out: Vec<Action>,
}

impl Context {
    pub fn new(policy: Policy, registry: Registry, arbiter: impl Arbiter + 'static) -> Self {
        Self {
            policy,
            guests: Guests::new(registry),
            arbiter: Box::new(arbiter),
            host: HostView::default(),
            out: Vec::new(),
        }
    }

    pub fn emit(&mut self, action: Action) {
        self.out.push(action);
    }

    pub fn send(&mut self, peer: &PeerId, message: HostMessage) {
        self.emit(Action::Send {
            peer: peer.clone(),
            message,
        });
    }

    pub fn answer(&mut self, peer: &PeerId, id: u64, decision: Decision) {
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

        self.send(peer, message);
    }

    pub fn close(&mut self, guest: GuestId, peer: &PeerId) {
        if let Some(state) = self.guests.state_mut(guest) {
            state.session = None;
        }

        self.emit(Action::Close { peer: peer.clone() });
    }

    pub fn close_unknown(&mut self, peer: &PeerId) {
        self.emit(Action::Close { peer: peer.clone() });
    }

    pub fn drain(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.out)
    }

    #[must_use]
    pub fn name(&self, guest: GuestId) -> &str {
        self.guests.name(guest)
    }

    #[must_use]
    pub fn ceiling(&self, guest: GuestId) -> u64 {
        self.guests.ceiling(guest)
    }

    #[must_use]
    pub const fn unsized_deny(&self) -> Decision {
        Decision::Deny {
            reason: DenyReason::Policy,
            retry_after: Some(self.policy.grant_min_interval.get()),
        }
    }

    pub fn arbitrate(&mut self, now: Timestamp, guest: GuestId, ask: Ask) -> Decision {
        let (asking, others) = self.guests.views(guest);

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

    #[must_use]
    pub fn size(&self, guest: GuestId) -> Option<u64> {
        self.guests.state(guest)?.size
    }

    pub fn request(&mut self, said: Said<'_>, id: u64, ask: Result<Ask, Decision>) {
        let ask = match ask {
            Ok(ask) => ask,
            Err(decision) => {
                self.answer(said.peer, id, decision);

                return;
            }
        };

        let decision = self.arbitrate(said.now, said.guest, ask);

        let Decision::Grant { target } = decision else {
            self.answer(said.peer, id, decision);

            return;
        };

        let target = target.min(self.ceiling(said.guest));

        self.answer(said.peer, id, Decision::Grant { target });

        if matches!(ask, Ask::Grow { .. })
            && let Some(state) = self.guests.state_mut(said.guest)
        {
            state.granted = Some(said.now);
        }

        self.actuate(said.now, said.guest, target);
    }

    pub fn actuate(&mut self, now: Timestamp, guest: GuestId, target: u64) {
        let deadline = now.saturating_add(self.policy.reconcile_deadline.get());

        let Some(state) = self.guests.state_mut(guest) else {
            return;
        };

        let Some(size) = state.size else {
            return;
        };

        if target == size {
            if state.reconcile.take().is_some() {
                self.emit(Action::SetRequested {
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

        self.emit(Action::SetRequested {
            guest,
            bytes: target,
        });
    }

    pub fn settled(&mut self, guest: GuestId, size: u64) {
        let name = self.guests.name(guest).to_owned();

        let Some(state) = self.guests.state_mut(guest) else {
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
            guest = name,
            size,
            target = reconcile.target,
            "an actuation completed"
        );
    }
}
