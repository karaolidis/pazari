use pazari_types::Timestamp;
use tracing::{debug, warn};

use crate::engine::{
    Engine,
    counters::Events,
    effects::Effects,
    event::Snapshot,
    phase::{Phase, State},
};

impl Engine {
    pub(in crate::engine) fn on_tick(
        &mut self,
        now: Timestamp,
        snapshot: Snapshot,
        fx: &mut Effects,
    ) {
        let events = self
            .counters
            .observe(snapshot.high_events, snapshot.max_events, false);

        self.world.observe(snapshot);

        self.note_breaches(events);
        self.on_total_changed(now, snapshot.total_mem, fx);

        if events.throttled {
            self.brake(now, fx);
        }

        let pressure = self.counters.take();

        self.transition(now, fx, |phase, step| phase.advance(step, pressure));
        self.transition(now, fx, |phase, step| phase.on_interval(step, pressure));

        self.emit_status(now, fx);
    }

    pub(in crate::engine) fn on_throttle_wake(
        &mut self,
        now: Timestamp,
        high_events: u64,
        max_events: u64,
        fx: &mut Effects,
    ) {
        let events = self.counters.observe(high_events, max_events, true);

        self.note_breaches(events);

        if events.throttled {
            self.brake(now, fx);
        }
    }

    fn note_breaches(&self, events: Events) {
        let Some(breaches) = events.breaches else {
            return;
        };

        warn!(
            breaches = breaches.get(),
            available = ?self.world.guest().available(),
            usage = self.world.guest().usage(),
            memory_max = self.world.limits().max(),
            total_mem = self.world.limits().total_mem(),
            phase = %self.phase,
            "workload hit memory.max",
        );
    }

    fn brake(&mut self, now: Timestamp, fx: &mut Effects) {
        debug!(
            usage = self.world.guest().usage(),
            high = self.world.limits().high(),
            total_mem = self.world.limits().total_mem(),
            released = self.world.limits().released(),
            "workload throttled at memory.high",
        );

        self.transition(now, fx, Phase::brake);
    }
}
