mod exhausted;
mod growing;
mod releasing;
mod steady;
mod titrating;

use std::time::Duration;

use pazari_proto::DenyReason;

use crate::engine::{
    Mode,
    context::Context,
    phase::{
        exhausted::Exhausted, growing::Growing, releasing::Releasing, steady::Steady,
        titrating::Titrating,
    },
    time::Timestamp,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Titrating(Titrating),
    Steady(Steady),
    Growing(Growing),
    Releasing(Releasing),
    Exhausted(Exhausted),
}

pub trait State {
    fn advance(self, ctx: &mut Context, now: Timestamp) -> Phase;

    fn brake(self, ctx: &mut Context, now: Timestamp) -> Phase;

    fn on_plugged(self, ctx: &mut Context, now: Timestamp, changed: bool) -> Phase;

    fn on_grant(self, ctx: &mut Context, now: Timestamp, id: u64, target: u64) -> Phase;

    fn on_deny(
        self,
        ctx: &mut Context,
        now: Timestamp,
        id: u64,
        reason: DenyReason,
        retry_after: Option<Duration>,
    ) -> Phase;

    fn on_reclaim_request(self, ctx: &mut Context, id: u64, desired: u64) -> Phase;

    fn on_reclaim_finished(self, ctx: &mut Context, now: Timestamp, reclaimed: u64) -> Phase;

    fn on_interval(self, ctx: &mut Context, now: Timestamp, throttled: bool) -> Phase;

    fn on_ceiling_raised(self, ctx: &mut Context, now: Timestamp) -> Option<Phase>;

    fn on_connected(self) -> Phase;

    fn on_disconnected(self, ctx: &mut Context, now: Timestamp) -> Phase;

    fn resume(self, ctx: &mut Context, now: Timestamp) -> Phase;
}

impl Phase {
    pub fn start() -> Self {
        Self::Titrating(Titrating::default())
    }

    pub const fn mode(self) -> Mode {
        match self {
            Self::Titrating(_) => Mode::Titrating,
            Self::Steady(_) => Mode::Steady,
            Self::Growing(_) => Mode::AwaitingGrant,
            Self::Releasing(_) => Mode::Releasing,
            Self::Exhausted(_) => Mode::Exhausted,
        }
    }

    pub const fn pending_request(self) -> Option<u64> {
        match self {
            Self::Growing(growing) => growing.pending_request(),
            Self::Releasing(releasing) => releasing.pending_request(),
            _ => None,
        }
    }

    pub fn advance(self, ctx: &mut Context, now: Timestamp) -> Self {
        match self {
            Self::Titrating(state) => state.advance(ctx, now),
            Self::Steady(state) => state.advance(ctx, now),
            Self::Growing(state) => state.advance(ctx, now),
            Self::Releasing(state) => state.advance(ctx, now),
            Self::Exhausted(state) => state.advance(ctx, now),
        }
    }

    pub fn brake(self, ctx: &mut Context, now: Timestamp) -> Self {
        match self {
            Self::Titrating(state) => state.brake(ctx, now),
            Self::Steady(state) => state.brake(ctx, now),
            Self::Growing(state) => state.brake(ctx, now),
            Self::Releasing(state) => state.brake(ctx, now),
            Self::Exhausted(state) => state.brake(ctx, now),
        }
    }

    pub fn on_plugged(self, ctx: &mut Context, now: Timestamp, changed: bool) -> Self {
        match self {
            Self::Titrating(state) => state.on_plugged(ctx, now, changed),
            Self::Steady(state) => state.on_plugged(ctx, now, changed),
            Self::Growing(state) => state.on_plugged(ctx, now, changed),
            Self::Releasing(state) => state.on_plugged(ctx, now, changed),
            Self::Exhausted(state) => state.on_plugged(ctx, now, changed),
        }
    }

    pub fn on_grant(self, ctx: &mut Context, now: Timestamp, id: u64, target: u64) -> Self {
        match self {
            Self::Titrating(state) => state.on_grant(ctx, now, id, target),
            Self::Steady(state) => state.on_grant(ctx, now, id, target),
            Self::Growing(state) => state.on_grant(ctx, now, id, target),
            Self::Releasing(state) => state.on_grant(ctx, now, id, target),
            Self::Exhausted(state) => state.on_grant(ctx, now, id, target),
        }
    }

    pub fn on_deny(
        self,
        ctx: &mut Context,
        now: Timestamp,
        id: u64,
        reason: DenyReason,
        retry_after: Option<Duration>,
    ) -> Self {
        match self {
            Self::Titrating(state) => state.on_deny(ctx, now, id, reason, retry_after),
            Self::Steady(state) => state.on_deny(ctx, now, id, reason, retry_after),
            Self::Growing(state) => state.on_deny(ctx, now, id, reason, retry_after),
            Self::Releasing(state) => state.on_deny(ctx, now, id, reason, retry_after),
            Self::Exhausted(state) => state.on_deny(ctx, now, id, reason, retry_after),
        }
    }

    pub fn on_reclaim_request(self, ctx: &mut Context, id: u64, desired: u64) -> Self {
        match self {
            Self::Titrating(state) => state.on_reclaim_request(ctx, id, desired),
            Self::Steady(state) => state.on_reclaim_request(ctx, id, desired),
            Self::Growing(state) => state.on_reclaim_request(ctx, id, desired),
            Self::Releasing(state) => state.on_reclaim_request(ctx, id, desired),
            Self::Exhausted(state) => state.on_reclaim_request(ctx, id, desired),
        }
    }

    pub fn on_reclaim_finished(self, ctx: &mut Context, now: Timestamp, reclaimed: u64) -> Self {
        match self {
            Self::Titrating(state) => state.on_reclaim_finished(ctx, now, reclaimed),
            Self::Steady(state) => state.on_reclaim_finished(ctx, now, reclaimed),
            Self::Growing(state) => state.on_reclaim_finished(ctx, now, reclaimed),
            Self::Releasing(state) => state.on_reclaim_finished(ctx, now, reclaimed),
            Self::Exhausted(state) => state.on_reclaim_finished(ctx, now, reclaimed),
        }
    }

    pub fn on_interval(self, ctx: &mut Context, now: Timestamp, throttled: bool) -> Self {
        match self {
            Self::Titrating(state) => state.on_interval(ctx, now, throttled),
            Self::Steady(state) => state.on_interval(ctx, now, throttled),
            Self::Growing(state) => state.on_interval(ctx, now, throttled),
            Self::Releasing(state) => state.on_interval(ctx, now, throttled),
            Self::Exhausted(state) => state.on_interval(ctx, now, throttled),
        }
    }

    pub fn on_ceiling_raised(self, ctx: &mut Context, now: Timestamp) -> Option<Self> {
        match self {
            Self::Titrating(state) => state.on_ceiling_raised(ctx, now),
            Self::Steady(state) => state.on_ceiling_raised(ctx, now),
            Self::Growing(state) => state.on_ceiling_raised(ctx, now),
            Self::Releasing(state) => state.on_ceiling_raised(ctx, now),
            Self::Exhausted(state) => state.on_ceiling_raised(ctx, now),
        }
    }

    pub fn on_connected(self) -> Self {
        match self {
            Self::Titrating(state) => state.on_connected(),
            Self::Steady(state) => state.on_connected(),
            Self::Growing(state) => state.on_connected(),
            Self::Releasing(state) => state.on_connected(),
            Self::Exhausted(state) => state.on_connected(),
        }
    }

    pub fn on_disconnected(self, ctx: &mut Context, now: Timestamp) -> Self {
        match self {
            Self::Titrating(state) => state.on_disconnected(ctx, now),
            Self::Steady(state) => state.on_disconnected(ctx, now),
            Self::Growing(state) => state.on_disconnected(ctx, now),
            Self::Releasing(state) => state.on_disconnected(ctx, now),
            Self::Exhausted(state) => state.on_disconnected(ctx, now),
        }
    }

    pub fn resume(self, ctx: &mut Context, now: Timestamp) -> Self {
        match self {
            Self::Titrating(state) => state.resume(ctx, now),
            Self::Steady(state) => state.resume(ctx, now),
            Self::Growing(state) => state.resume(ctx, now),
            Self::Releasing(state) => state.resume(ctx, now),
            Self::Exhausted(state) => state.resume(ctx, now),
        }
    }
}
