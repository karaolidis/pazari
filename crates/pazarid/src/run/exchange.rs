use std::collections::HashMap;

use tokio::sync::watch;

use crate::{
    broker::{Action, Broker, Event, GuestId},
    run::{metrics::Observer, peers::Peers},
};

pub type Target = watch::Sender<Option<u64>>;

pub struct Exchange {
    pub broker: Broker,
    pub peers: Peers,

    /// Every registered guest's device.
    pub targets: HashMap<GuestId, Target>,

    pub observer: Observer,
}

impl Exchange {
    pub fn drive(&mut self, event: Event) {
        self.observer.observe_event(&event);

        let actions = self.broker.handle(event);

        self.observer.observe_actions(&actions);
        self.dispatch(&actions);
    }

    pub fn refresh(&self) {
        self.observer.refresh(&self.broker);
    }

    fn dispatch(&mut self, actions: &[Action]) {
        for action in actions {
            match action {
                Action::Send { peer, message } => self.peers.send(peer, message.clone()),
                Action::Close { peer } => self.peers.close(peer),
                Action::SetRequested { guest, bytes } => {
                    self.targets
                        .get(guest)
                        .expect("every registered guest has a device")
                        .send_replace(Some(*bytes));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use pazari_proto::HostMessage;

    use super::*;
    use crate::broker::testing::{BOOT, MIB, ONE, TWO, id, policy, registry, vsock};

    fn exchange(targets: HashMap<GuestId, Target>) -> Exchange {
        let (peers, _reports) = Peers::new();

        Exchange {
            broker: Broker::new(policy(), registry()),
            peers,
            targets,
            observer: Observer::default(),
        }
    }

    #[test]
    fn one_guest_that_cannot_be_actuated_does_not_swallow_the_batch() {
        let (one, deaf) = watch::channel(None);
        let (two, mut listening) = watch::channel(None);

        drop(deaf);

        exchange(HashMap::from([(id(ONE), one), (id(TWO), two)])).dispatch(&[
            Action::SetRequested {
                guest: id(ONE),
                bytes: BOOT,
            },
            Action::Send {
                peer: vsock(ONE),
                message: HostMessage::Ceiling { ceiling: BOOT },
            },
            Action::SetRequested {
                guest: id(TWO),
                bytes: 3072 * MIB,
            },
            Action::Close { peer: vsock(ONE) },
        ]);

        assert_eq!(*listening.borrow_and_update(), Some(3072 * MIB));
    }

    #[test]
    #[should_panic(expected = "every registered guest has a device")]
    fn a_size_for_a_guest_with_no_device_is_an_invariant_breach() {
        exchange(HashMap::from([(id(ONE), watch::channel(None).0)])).dispatch(&[
            Action::SetRequested {
                guest: id(TWO),
                bytes: BOOT,
            },
        ]);
    }
}
