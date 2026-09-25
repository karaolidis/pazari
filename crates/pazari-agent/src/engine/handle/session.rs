use std::time::Duration;

use pazari_proto::{GuestMessage, Status};
use pazari_types::Timestamp;
use tracing::debug;

use crate::engine::{
    Engine,
    effects::Effects,
    phase::{Phase, State, Step},
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
        self.transition(now, fx, Phase::on_connected);
        self.emit_status(now, fx);

        if !self.adopt_ceiling(now, ceiling, fx) {
            self.transition(now, fx, Phase::resume);
        }
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
    ) -> bool {
        if !self.world.raise_ceiling(ceiling) {
            return false;
        }

        debug!(
            ceiling,
            total_mem = self.world.limits().total_mem(),
            "ceiling raised",
        );

        let mut step = Step {
            world: &mut self.world,
            fx,
            now,
        };

        let Some(phase) = self.phase.on_ceiling_raised(&mut step) else {
            return false;
        };

        self.phase = phase;

        true
    }

    pub(in crate::engine) fn emit_status(&mut self, now: Timestamp, fx: &mut Effects) {
        if !self.world.status_due(now) {
            return;
        }

        let counters = self.counters.seen();
        let reading = self.world.guest().reading().unwrap_or_default();

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
