use pazari_types::Timestamp;
use tracing::{debug, info, warn};

use crate::broker::{Broker, action::Action, registry::GuestId, session::Session};

impl Broker {
    pub(in crate::broker) fn on_tick(&mut self, now: Timestamp) {
        for slot in 0..self.ctx.guests.len() {
            let guest = GuestId::from_index(slot);

            self.tick_reconcile(now, guest);
            self.tick_reclaim(now, guest);
            self.tick_staleness(now, guest);
        }

        self.tick_revocation(now);
    }

    fn tick_revocation(&mut self, now: Timestamp) {
        let Some(revocation) = self.ctx.policy.revocation else {
            return;
        };

        let Some(psi) = self.ctx.host.psi else {
            return;
        };

        if f64::from(psi) < revocation.psi.get() {
            return;
        }

        let mut candidates = Vec::with_capacity(self.ctx.guests.len());

        for (id, entry) in self.ctx.guests.iter() {
            let asked = entry
                .state
                .session
                .as_ref()
                .and_then(Session::negotiated)
                .is_none_or(|session| session.reclaim.is_some());

            if entry.state.size.is_some() && !asked {
                candidates.push(entry.view(id));
            }
        }

        let Some((guest, desired)) = self.ctx.arbiter.revoke(&candidates, revocation) else {
            return;
        };

        warn!(
            guest = self.ctx.name(guest),
            psi,
            threshold = revocation.psi.get(),
            "host is under memory pressure; reclaiming",
        );

        self.on_reclaim(now, guest, desired);
    }

    fn tick_reconcile(&mut self, now: Timestamp, guest: GuestId) {
        let revert = self.ctx.policy.shrink_revert;
        let ceiling = self.ctx.ceiling(guest);
        let name = self.ctx.name(guest).to_owned();

        let Some(state) = self.ctx.guests.state_mut(guest) else {
            return;
        };

        let Some(reconcile) = state.reconcile else {
            return;
        };

        if now < reconcile.deadline {
            return;
        }

        state.reconcile = None;

        let Some(size) = state.size else {
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
            self.ctx.emit(Action::SetRequested {
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
        let name = self.ctx.name(guest).to_owned();

        let Some(session) = self.ctx.guests.negotiated_mut(guest) else {
            return;
        };

        let Some(reclaim) = session.reclaim else {
            return;
        };

        if now < reclaim.deadline {
            return;
        }

        session.reclaim = None;

        if reclaim.refused {
            debug!(guest = name, id = reclaim.id, "guest left reclaim backoff",);

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
        let window = self
            .ctx
            .policy
            .welcome_status_interval
            .get()
            .saturating_mul(self.ctx.policy.stale_after.get());

        let name = self.ctx.name(guest).to_owned();

        let Some(session) = self.ctx.guests.negotiated_mut(guest) else {
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
