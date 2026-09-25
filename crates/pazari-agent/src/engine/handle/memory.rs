use pazari_types::Timestamp;

use crate::engine::{Engine, effects::Effects, phase::State};

impl Engine {
    pub(in crate::engine) fn on_total_changed(
        &mut self,
        now: Timestamp,
        total_mem: u64,
        fx: &mut Effects,
    ) {
        let changed = self.world.set_total_mem(total_mem);

        self.transition(now, fx, |phase, step| phase.on_total_changed(step, changed));
    }
}
