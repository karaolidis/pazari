use std::time::Duration;

use pazari_proto::{GuestMessage, Status};
use pazari_types::Timestamp;
use tracing::debug;

use crate::engine::{
    Engine,
    effects::Effects,
    limits::Moved,
    phase::{Phase, State},
};

impl Engine {
    pub(in crate::engine) fn on_connected(
        &mut self,
        now: Timestamp,
        ceiling: u64,
        status_interval: Duration,
        hotplugged: u64,
        fx: &mut Effects,
    ) {
        self.world.connect(status_interval, hotplugged);
        self.emit_status(now, fx);

        self.adopt_ceiling(now, ceiling, fx);
        self.transition(now, fx, Phase::on_connected);
    }

    pub(in crate::engine) fn on_disconnected(&mut self, now: Timestamp, fx: &mut Effects) {
        self.world.disconnect();

        self.transition(now, fx, Phase::on_disconnected);
    }

    pub(in crate::engine) fn adopt_ceiling(
        &mut self,
        now: Timestamp,
        ceiling: u64,
        fx: &mut Effects,
    ) {
        if self.world.set_ceiling(ceiling) != Moved::Grew {
            return;
        }

        debug!(
            ceiling,
            total_mem = self.world.limits().total_mem(),
            "ceiling raised",
        );

        self.transition(now, fx, Phase::on_ceiling_raised);
    }

    /// Sends a status once one is due, from the first reading on.
    pub(in crate::engine) fn emit_status(&mut self, now: Timestamp, fx: &mut Effects) {
        let (Some(reading), Some(counters)) = (self.world.guest().reading(), self.counters.seen())
        else {
            return;
        };

        if !self.world.status_due(now) {
            return;
        }

        self.world.send(
            GuestMessage::Status(Status {
                total_mem: self.world.limits().total_mem(),
                available: reading.available,
                high_events: counters.high,
                max_events: counters.max,
                psi_some10: reading.psi_some10,
                psi_full10: reading.psi_full10,
                swap_used: reading.swap_used,
                workload_usage: reading.workload_usage,
            }),
            fx,
        );
    }
}
