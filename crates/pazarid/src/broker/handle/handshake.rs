use pazari_transport::PeerId;
use tracing::{debug, info, warn};

use crate::broker::{Broker, action::Action, session::Session};

impl Broker {
    pub(in crate::broker) fn on_connected(&mut self, peer: PeerId) {
        let Some(guest) = self.ctx.guests.lookup(&peer) else {
            warn!(%peer, "a connection from a peer no guest answers to");
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
                "the guest connected again over a connection the host still held",
            );
        } else {
            debug!(guest = self.ctx.name(guest), %peer, "a guest connected");
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
            debug!(guest = self.ctx.name(guest), %peer, "a guest disconnected");
        }
    }
}
