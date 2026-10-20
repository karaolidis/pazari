use pazari_types::Timestamp;
use tracing::{debug, info, warn};

use crate::broker::{
    Broker,
    action::Action,
    effects::Effects,
    guest::Entry,
    phase::{Reply, Reporting},
    policy::StalledShrinks,
    registry::GuestId,
    session::Session,
};

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

        if psi < revocation.psi.get() {
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
        let stalled_shrinks = self.world.policy().stalled_shrinks;

        let Entry { spec, state } = self.world.guests_mut().get_mut(guest);

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
                guest = spec.name,
                target = reconcile.target,
                size,
                "grow timed out; accepting the size it reached",
            );

            return;
        }

        if stalled_shrinks == StalledShrinks::Revert {
            fx.emit(Action::SetRequested {
                guest,
                bytes: size.min(spec.ceiling.get()),
            });
        }

        warn!(
            guest = spec.name,
            target = reconcile.target,
            size,
            ?stalled_shrinks,
            "shrink timed out",
        );
    }

    fn tick_reclaim(&mut self, now: Timestamp, guest: GuestId) {
        let Entry { spec, state } = self.world.guests_mut().get_mut(guest);

        let Some(session) = state.session.as_mut().and_then(Session::negotiated_mut) else {
            return;
        };

        let Some(reclaim) = session.reclaim else {
            return;
        };

        if !reclaim.deadline.reached(now) {
            return;
        }

        session.reclaim = None;

        match reclaim.reply {
            Reply::Refused => {
                debug!(guest = spec.name, id = %reclaim.id, "guest left reclaim backoff");
            }
            Reply::Pending | Reply::Committed(_) => {
                warn!(
                    guest = spec.name,
                    id = %reclaim.id,
                    desired = reclaim.desired,
                    reply = ?reclaim.reply,
                    "reclaim timed out",
                );
            }
        }
    }

    fn tick_staleness(&mut self, now: Timestamp, guest: GuestId) {
        let policy = self.world.policy();
        let window = policy
            .status_interval
            .get()
            .saturating_mul(policy.stale_after.get());

        let Entry { spec, state } = self.world.guests_mut().get_mut(guest);

        let Some(session) = state.session.as_mut().and_then(Session::negotiated_mut) else {
            return;
        };

        if session.reporting == Reporting::Stale {
            return;
        }

        let quiet = now.saturating_duration_since(session.heard);

        if quiet <= window {
            return;
        }

        session.reporting = Reporting::Stale;

        warn!(guest = spec.name, ?quiet, "guest stopped sending status");
    }
}
