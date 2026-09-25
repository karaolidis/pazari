use std::mem;

use pazari_proto::HostMessage;
use pazari_transport::PeerId;

use crate::broker::action::Action;

/// What a transition decided to do to the guests and tell them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Effects {
    actions: Vec<Action>,
}

impl Effects {
    pub fn emit(&mut self, action: Action) {
        self.actions.push(action);
    }

    pub fn send(&mut self, peer: &PeerId, message: HostMessage) {
        self.emit(Action::Send {
            peer: peer.clone(),
            message,
        });
    }

    pub fn close(&mut self, peer: &PeerId) {
        self.emit(Action::Close { peer: peer.clone() });
    }

    pub fn drain(&mut self) -> Vec<Action> {
        mem::take(&mut self.actions)
    }
}
