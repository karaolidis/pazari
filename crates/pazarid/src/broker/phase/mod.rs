mod awaiting_hello;
mod negotiated;

use std::num::NonZeroU64;

use enum_dispatch::enum_dispatch;
use pazari_proto::{GuestMessage, HostMessage, Status};
use pazari_transport::PeerId;
use pazari_types::Timestamp;

pub use crate::broker::phase::{
    awaiting_hello::AwaitingHello,
    negotiated::{Negotiated, Reclaim},
};
use crate::broker::{
    arbiter::Decision, effects::Effects, policy::Policy, registry::GuestId, world::World,
};

#[derive(Debug)]
pub struct Step<'a> {
    pub world: &'a mut World,
    pub fx: &'a mut Effects,
    pub now: Timestamp,
    pub guest: GuestId,
    pub peer: &'a PeerId,
}

impl Step<'_> {
    pub const fn policy(&self) -> &Policy {
        self.world.policy()
    }

    pub fn name(&self) -> &str {
        self.world.name(self.guest)
    }

    pub fn ceiling(&self) -> Option<NonZeroU64> {
        self.world.ceiling(self.guest)
    }

    pub fn size(&self) -> Option<u64> {
        self.world.size(self.guest)
    }

    pub fn send(&mut self, message: HostMessage) {
        self.fx.send(self.peer, message);
    }

    pub fn answer(&mut self, id: u64, decision: Decision) {
        self.world.answer(self.peer, id, decision, self.fx);
    }

    pub fn close(&mut self) {
        self.world.close(self.guest, self.peer, self.fx);
    }
}

#[enum_dispatch]
pub trait State: Copy {
    fn stay(self) -> Phase;

    fn refuse(self, step: &mut Step<'_>, what: &'static str) -> Phase;

    fn on_hello(self, step: &mut Step<'_>, _message: GuestMessage) -> Phase {
        self.refuse(step, "hello")
    }

    fn on_grant_request(self, step: &mut Step<'_>, _id: u64, _target: u64) -> Phase {
        self.refuse(step, "grant_request")
    }

    fn on_offer_release(self, step: &mut Step<'_>, _id: u64, _releasable: u64) -> Phase {
        self.refuse(step, "offer_release")
    }

    fn on_reclaim_ack(self, step: &mut Step<'_>, _id: u64, _will_release: u64) -> Phase {
        self.refuse(step, "reclaim_ack")
    }

    fn on_reclaim_nak(self, step: &mut Step<'_>, _id: u64, _reason: &str) -> Phase {
        self.refuse(step, "reclaim_nak")
    }

    fn on_status(self, step: &mut Step<'_>, _status: Status) -> Phase {
        self.refuse(step, "status")
    }

    fn on_bye(self, step: &mut Step<'_>) -> Phase {
        self.refuse(step, "bye")
    }
}

#[enum_dispatch(State)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Phase {
    AwaitingHello(AwaitingHello),
    Negotiated(Negotiated),
}

impl Phase {
    #[must_use]
    pub const fn start() -> Self {
        Self::AwaitingHello(AwaitingHello::new())
    }

    #[must_use]
    pub const fn negotiated(&self) -> Option<&Negotiated> {
        match self {
            Self::AwaitingHello(_) => None,
            Self::Negotiated(negotiated) => Some(negotiated),
        }
    }

    pub const fn negotiated_mut(&mut self) -> Option<&mut Negotiated> {
        match self {
            Self::AwaitingHello(_) => None,
            Self::Negotiated(negotiated) => Some(negotiated),
        }
    }
}
