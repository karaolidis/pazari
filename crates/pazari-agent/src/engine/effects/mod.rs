use std::mem;

use crate::engine::action::Action;

/// What a transition decided to do to the guest and tell the host.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Effects {
    actions: Vec<Action>,
}

impl Effects {
    pub fn emit(&mut self, action: Action) {
        self.actions.push(action);
    }

    pub fn drain(&mut self) -> Vec<Action> {
        mem::take(&mut self.actions)
    }
}
