use pazari_transport::PeerId;
use pazari_types::Timestamp;
use tracing::{debug, info, warn};

use crate::broker::{Broker, action::Action, phase::Phase, registry::GuestId, session::Session};

impl Broker {
    pub(in crate::broker) fn on_connected(&mut self, peer: PeerId) {
        let Some(guest) = self.ctx.guests.lookup(&peer) else {
            warn!(%peer, "connection from an unknown peer");
            self.ctx.emit(Action::Close { peer });

            return;
        };

        let Some(state) = self.ctx.guests.state_mut(guest) else {
            return;
        };

        let replaced = state.session.replace(Session::new(peer.clone())).is_some();

        if replaced {
            info!(
                guest = self.ctx.name(guest),
                %peer,
                "guest reconnected while the previous connection was still open",
            );
        } else {
            debug!(guest = self.ctx.name(guest), %peer, "guest connected");
        }
    }

    pub(in crate::broker) fn on_size(
        &mut self,
        now: Timestamp,
        guest: GuestId,
        size: u64,
        hotplugged: u64,
    ) {
        self.ctx.settled(guest, size, hotplugged);

        let Some(session) = self.ctx.guests.session(guest) else {
            return;
        };

        let peer = session.peer.clone();

        match session.phase {
            Phase::AwaitingHello(awaiting) => {
                let next = awaiting.welcome(&mut self.ctx, now, guest, &peer);

                if let Some(session) = self.ctx.guests.session_mut(guest) {
                    session.phase = next;
                }
            }
            Phase::Negotiated(_) => self.ctx.tell_geometry(guest, &peer),
        }
    }

    pub(in crate::broker) fn on_disconnected(&mut self, peer: &PeerId) {
        let Some(guest) = self.ctx.guests.lookup(peer) else {
            return;
        };

        let Some(state) = self.ctx.guests.state_mut(guest) else {
            return;
        };

        if state.session.take().is_some() {
            debug!(guest = self.ctx.name(guest), %peer, "guest disconnected");
        }
    }
}
