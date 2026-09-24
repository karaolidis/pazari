mod exhausted;
mod growing;
mod releasing;
mod steady;
mod titrating;

use std::time::Duration;

use enum_dispatch::enum_dispatch;
use pazari_proto::DenyReason;
use pazari_types::Timestamp;

use crate::engine::{
    Mode,
    context::Context,
    phase::{
        exhausted::Exhausted, growing::Growing, releasing::Releasing, steady::Steady,
        titrating::Titrating,
    },
};

#[enum_dispatch]
pub trait State: Copy {
    fn stay(self) -> Phase;

    fn busy(self) -> &'static str;

    fn advance(self, _ctx: &mut Context, _now: Timestamp) -> Phase {
        self.stay()
    }

    fn brake(self, ctx: &mut Context, now: Timestamp) -> Phase {
        Growing::start(ctx, now, None)
    }

    fn on_total_changed(self, ctx: &mut Context, _now: Timestamp, changed: bool) -> Phase {
        if changed {
            ctx.reapply_limits();
        }

        self.stay()
    }

    fn on_grant(self, _ctx: &mut Context, _now: Timestamp, _id: u64, _target: u64) -> Phase {
        self.stay()
    }

    fn on_deny(
        self,
        _ctx: &mut Context,
        _now: Timestamp,
        _id: u64,
        _reason: DenyReason,
        _retry_after: Option<Duration>,
    ) -> Phase {
        self.stay()
    }

    fn on_reclaim_request(self, ctx: &mut Context, id: u64, _desired: u64) -> Phase {
        ctx.nak(id, self.busy());

        self.stay()
    }

    fn on_reclaim_finished(self, _ctx: &mut Context, _now: Timestamp, _reclaimed: u64) -> Phase {
        self.stay()
    }

    fn on_interval(self, ctx: &mut Context, _now: Timestamp, _throttled: bool) -> Phase {
        ctx.idle.reset();

        self.stay()
    }

    fn on_ceiling_raised(self, _ctx: &mut Context, _now: Timestamp) -> Option<Phase> {
        None
    }

    fn on_connected(self) -> Phase {
        self.stay()
    }

    fn on_disconnected(self, _ctx: &mut Context, _now: Timestamp) -> Phase {
        self.stay()
    }

    fn resume(self, ctx: &mut Context, now: Timestamp) -> Phase {
        Growing::start(ctx, now, None)
    }
}

#[enum_dispatch(State)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Titrating(Titrating),
    Steady(Steady),
    Growing(Growing),
    Releasing(Releasing),
    Exhausted(Exhausted),
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
}
