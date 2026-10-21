use std::mem;

use crate::engine::{action::Action, transition::Transition};

/// What the engine decided while handling one event.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Outcome {
    /// What to do to the guest and tell the host.
    pub actions: Vec<Action>,

    /// The transitions it made on the way.
    pub transitions: Vec<Transition>,
}

/// What a transition decided, gathered as it goes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Effects {
    outcome: Outcome,
}

impl Effects {
    pub fn emit(&mut self, action: Action) {
        self.outcome.actions.push(action);
    }

    pub fn record(&mut self, transition: Transition) {
        self.outcome.transitions.push(transition);
    }

    pub fn drain(&mut self) -> Outcome {
        mem::take(&mut self.outcome)
    }
}
