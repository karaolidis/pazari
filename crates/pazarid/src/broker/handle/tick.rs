use pazari_types::Timestamp;
use tracing::{debug, info, warn};

use crate::broker::{Broker, action::Action, effects::Effects, registry::GuestId};

impl Broker {
    pub(in crate::broker) fn on_tick(&mut self, now: Timestamp, fx: &mut Effects) {
        for slot in 0..self.world.guests().len() {
            let guest = GuestId::from_index(slot);

            self.tick_reconcile(now, guest, fx);
            self.tick_reclaim(now, guest);
            self.tick_staleness(now, guest);
        }

        self.tick_revocation(now, fx);
    }

    fn tick_revocation(&mut self, now: Timestamp, fx: &mut Effects) {
        let Some(revocation) = self.world.policy().revocation else {
            return;
        };

        let Some(psi) = self.world.host().psi else {
            return;
        };

        if f64::from(psi) < revocation.psi.get() {
            return;
        }

        let Some((guest, desired)) = self.world.revoke() else {
            return;
        };

        warn!(
            guest = self.world.name(guest),
            psi,
            threshold = revocation.psi.get(),
            "host is under memory pressure; reclaiming",
        );

        self.on_reclaim(now, guest, desired, fx);
    }

    fn tick_reconcile(&mut self, now: Timestamp, guest: GuestId, fx: &mut Effects) {
        let revert = self.world.policy().shrink_revert;
        let ceiling = self.world.ceiling(guest);
        let name = self.world.name(guest).to_owned();

        let Some(state) = self.world.guests_mut().state_mut(guest) else {
            return;
        };

        let Some(reconcile) = state.reconcile else {
            return;
        };

        if !reconcile.deadline.reached(now) {
            return;
        }

        state.reconcile = None;

        let Some(size) = state.size() else {
            return;
        };

        if !reconcile.is_shrink() {
            info!(
                guest = name,
                target = reconcile.target,
                size,
                "grow timed out; accepting the size it reached",
            );

            return;
        }

        if revert && let Some(ceiling) = ceiling {
            fx.emit(Action::SetRequested {
                guest,
                bytes: size.min(ceiling.get()),
            });
        }

        warn!(
            guest = name,
            target = reconcile.target,
            size,
            reverted = revert,
            "shrink timed out",
        );
    }

    fn tick_reclaim(&mut self, now: Timestamp, guest: GuestId) {
        let name = self.world.name(guest).to_owned();

        let Some(session) = self.world.guests_mut().negotiated_mut(guest) else {
            return;
        };

        let Some(reclaim) = session.reclaim else {
            return;
        };

        if !reclaim.deadline.reached(now) {
            return;
        }

        session.reclaim = None;

        if reclaim.refused {
            debug!(guest = name, id = reclaim.id, "guest left reclaim backoff");

            return;
        }

        warn!(
            guest = name,
            id = reclaim.id,
            desired = reclaim.desired,
            committed = ?reclaim.committed,
            "reclaim timed out",
        );
    }

    fn tick_staleness(&mut self, now: Timestamp, guest: GuestId) {
        let policy = self.world.policy();
        let window = policy
            .welcome_status_interval
            .get()
            .saturating_mul(policy.stale_after.get());

        let name = self.world.name(guest).to_owned();

        let Some(session) = self.world.guests_mut().negotiated_mut(guest) else {
            return;
        };

        if session.stale {
            return;
        }

        let quiet = now.saturating_duration_since(session.heard);

        if quiet <= window {
            return;
        }

        session.stale = true;

        warn!(guest = name, ?quiet, "guest stopped sending status");
    }
}
