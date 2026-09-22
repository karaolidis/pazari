mod awaiting_hello;
mod negotiated;

use enum_dispatch::enum_dispatch;
use pazari_proto::{GrantReason, GuestMessage, Status, Urgency};
use pazari_transport::PeerId;
use pazari_types::Timestamp;

pub use crate::broker::phase::{
    awaiting_hello::AwaitingHello,
    negotiated::{Negotiated, Reclaim},
};
use crate::broker::{context::Context, registry::GuestId};

#[derive(Debug, Clone, Copy)]
pub struct Said<'a> {
    pub now: Timestamp,
    pub guest: GuestId,
    pub peer: &'a PeerId,
}

#[enum_dispatch]
pub trait State: Copy {
    fn stay(self) -> Phase;

    fn refuse(self, ctx: &mut Context, said: Said<'_>, what: &'static str) -> Phase;

    fn on_hello(self, ctx: &mut Context, said: Said<'_>, _message: GuestMessage) -> Phase {
        self.refuse(ctx, said, "hello")
    }

    fn on_grant_request(
        self,
        ctx: &mut Context,
        said: Said<'_>,
        _id: u64,
        _target: u64,
        _reason: GrantReason,
        _urgency: Urgency,
    ) -> Phase {
        self.refuse(ctx, said, "grant_request")
    }

    fn on_offer_release(
        self,
        ctx: &mut Context,
        said: Said<'_>,
        _id: u64,
        _releasable: u64,
    ) -> Phase {
        self.refuse(ctx, said, "offer_release")
    }

    fn on_reclaim_ack(
        self,
        ctx: &mut Context,
        said: Said<'_>,
        _id: u64,
        _will_release: u64,
    ) -> Phase {
        self.refuse(ctx, said, "reclaim_ack")
    }

    fn on_reclaim_nak(self, ctx: &mut Context, said: Said<'_>, _id: u64, _reason: &str) -> Phase {
        self.refuse(ctx, said, "reclaim_nak")
    }

    fn on_status(self, ctx: &mut Context, said: Said<'_>, _status: Status) -> Phase {
        self.refuse(ctx, said, "status")
    }

    fn on_bye(self, ctx: &mut Context, said: Said<'_>) -> Phase {
        self.refuse(ctx, said, "bye")
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
        Self::AwaitingHello(AwaitingHello)
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
