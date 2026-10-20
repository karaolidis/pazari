mod awaiting_hello;
mod negotiated;

use std::num::NonZeroU64;

use enum_dispatch::enum_dispatch;
use pazari_proto::{HostMessage, ReclaimId, RequestId, Status};
use pazari_transport::PeerId;
use pazari_types::Timestamp;

pub use crate::broker::phase::{
    awaiting_hello::AwaitingHello,
    negotiated::{Negotiated, Reclaim, Reply, Reporting},
};
use crate::broker::{
    arbiter::Decision, effects::Effects, guest::Geometry, policy::Policy, registry::GuestId,
    world::World,
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

    pub fn ceiling(&self) -> NonZeroU64 {
        self.world.ceiling(self.guest)
    }

    pub fn geometry(&self) -> Option<Geometry> {
        self.world.geometry(self.guest)
    }

    pub fn requested(&self) -> Option<u64> {
        self.world.requested(self.guest)
    }

    pub fn send(&mut self, message: HostMessage) {
        self.fx.send(self.peer, message);
    }

    pub fn answer(&mut self, id: RequestId, decision: Decision) {
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

    fn on_hello(
        self,
        step: &mut Step<'_>,
        _protocol: u16,
        _agent_version: &str,
        _block_size: NonZeroU64,
        _total_mem: u64,
    ) -> Phase {
        self.refuse(step, "hello")
    }

    fn on_grant_request(self, step: &mut Step<'_>, _id: RequestId, _target: u64) -> Phase {
        self.refuse(step, "grant_request")
    }

    fn on_offer_release(self, step: &mut Step<'_>, _id: RequestId, _releasable: u64) -> Phase {
        self.refuse(step, "offer_release")
    }

    fn on_reclaim_ack(self, step: &mut Step<'_>, _id: ReclaimId, _will_release: u64) -> Phase {
        self.refuse(step, "reclaim_ack")
    }

    fn on_reclaim_nak(self, step: &mut Step<'_>, _id: ReclaimId, _reason: &str) -> Phase {
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
