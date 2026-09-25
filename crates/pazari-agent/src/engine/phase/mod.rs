mod exhausted;
mod growing;
mod releasing;
mod steady;

use std::{fmt, time::Duration};

use enum_dispatch::enum_dispatch;
use pazari_proto::{DenyReason, GuestMessage};
use pazari_types::Timestamp;

pub use crate::engine::phase::{
    exhausted::Exhausted, growing::Growing, releasing::Releasing, steady::Steady,
};
use crate::engine::{
    action::Action,
    config::EngineConfig,
    counters::Pressure,
    effects::Effects,
    limits::{Brake, Limits},
    world::{Guest, World},
};

#[derive(Debug)]
pub struct Step<'a> {
    pub world: &'a mut World,
    pub fx: &'a mut Effects,
    pub now: Timestamp,
}

impl Step<'_> {
    pub const fn config(&self) -> &EngineConfig {
        &self.world.config
    }

    pub const fn limits(&self) -> Limits {
        self.world.limits()
    }

    pub const fn guest(&self) -> Guest {
        self.world.guest()
    }

    pub fn brake(&mut self, brake: Brake) {
        self.world.brake(brake, self.fx);
    }

    pub fn emit(&mut self, action: Action) {
        self.fx.emit(action);
    }

    pub fn send(&mut self, message: GuestMessage) {
        self.world.send(message, self.fx);
    }

    pub fn nak(&mut self, id: u64, reason: &str) {
        self.world.nak(id, reason, self.fx);
    }
}

pub fn spare(step: &Step<'_>, pressure: Pressure) -> bool {
    !pressure.throttled
        && !pressure.breached
        && step
            .world
            .slack()
            .is_some_and(|slack| slack >= step.config().shrink_step.get())
}

#[enum_dispatch]
pub trait State: Copy {
    fn stay(self) -> Phase;

    fn busy(self) -> &'static str;

    fn brake(self, step: &mut Step<'_>) -> Phase;

    fn resume(self, step: &mut Step<'_>) -> Phase;

    fn advance(self, _step: &mut Step<'_>, _pressure: Pressure) -> Phase {
        self.stay()
    }

    fn on_interval(self, _step: &mut Step<'_>, _pressure: Pressure) -> Phase {
        self.stay()
    }

    fn on_total_changed(self, step: &mut Step<'_>, changed: bool) -> Phase {
        if changed {
            step.world.rebrake(step.fx);
        }

        self.stay()
    }

    fn on_grant(self, _step: &mut Step<'_>, _id: u64, _target: u64) -> Phase {
        self.stay()
    }

    fn on_deny(
        self,
        _step: &mut Step<'_>,
        _id: u64,
        _reason: DenyReason,
        _retry_after: Option<Duration>,
    ) -> Phase {
        self.stay()
    }

    fn on_reclaim_request(self, step: &mut Step<'_>, id: u64, _desired: u64) -> Phase {
        step.nak(id, self.busy());

        self.stay()
    }

    fn on_reclaim_finished(self, _step: &mut Step<'_>, _reclaimed: u64) -> Phase {
        self.stay()
    }

    fn on_ceiling_raised(self, _step: &mut Step<'_>) -> Option<Phase> {
        None
    }

    fn on_connected(self, _step: &mut Step<'_>) -> Phase {
        self.stay()
    }

    fn on_disconnected(self, _step: &mut Step<'_>) -> Phase {
        self.stay()
    }
}

#[enum_dispatch(State)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Steady(Steady),
    Growing(Growing),
    Releasing(Releasing),
    Exhausted(Exhausted),
}

impl Phase {
    pub const NAMES: [&'static str; 4] = ["steady", "awaiting_grant", "releasing", "exhausted"];

    #[must_use]
    pub const fn start() -> Self {
        Self::Steady(Steady::start())
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Steady(_) => "steady",
            Self::Growing(_) => "awaiting_grant",
            Self::Releasing(_) => "releasing",
            Self::Exhausted(_) => "exhausted",
        }
    }

    #[must_use]
    pub const fn pending_request(self) -> Option<u64> {
        match self {
            Self::Growing(growing) => growing.pending_request(),
            Self::Releasing(releasing) => releasing.pending_request(),
            _ => None,
        }
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}
