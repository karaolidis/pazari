use pazari_types::Clock;

use crate::{
    broker::{Action, Event, GuestId},
    runtime::{
        actuator::{Actuator, Reading},
        error::RuntimeError,
    },
};

/// One guest's actuation between the broker and its memory device.
#[derive(Debug, Clone)]
pub struct Driver<A> {
    guest: GuestId,
    actuator: A,
    clock: Clock,
}

impl<A: Actuator> Driver<A> {
    #[must_use]
    pub const fn new(guest: GuestId, actuator: A, clock: Clock) -> Self {
        Self {
            guest,
            actuator,
            clock,
        }
    }

    #[must_use]
    pub const fn guest(&self) -> GuestId {
        self.guest
    }

    #[must_use]
    pub const fn actuator(&self) -> &A {
        &self.actuator
    }

    pub async fn apply(&self, actions: &[Action]) -> Result<(), RuntimeError> {
        for action in actions {
            let Action::SetRequested { guest, bytes } = action else {
                continue;
            };

            if *guest != self.guest {
                continue;
            }

            self.actuator.set_requested(*bytes).await?;
        }

        Ok(())
    }

    pub async fn seed(&self) -> Result<Event, RuntimeError> {
        Ok(self.sized(self.actuator.reading().await?))
    }

    pub async fn changed(&self) -> Result<Event, RuntimeError> {
        Ok(self.sized(self.actuator.changed().await?))
    }

    fn sized(&self, reading: Reading) -> Event {
        Event::Size {
            now: self.clock.now(),
            guest: self.guest,
            size: reading.size,
            hotplugged: reading.hotplugged,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        broker::testing::{BOOT, MIB, ONE, TWO, id, vsock},
        runtime::testing::{Completion, MockActuator},
    };

    fn driver(actuator: MockActuator) -> Driver<MockActuator> {
        Driver::new(id(ONE), actuator, Clock::start())
    }

    #[tokio::test]
    async fn an_actuation_lands_on_the_device_and_comes_back_as_a_size() {
        let driver = driver(MockActuator::new(BOOT));

        let Event::Size { guest, size, .. } = driver.seed().await.unwrap() else {
            panic!("the driver seeded with something other than a size");
        };

        assert_eq!(guest, id(ONE));
        assert_eq!(size, BOOT);

        driver
            .apply(&[Action::SetRequested {
                guest: id(ONE),
                bytes: 3072 * MIB,
            }])
            .await
            .unwrap();

        assert_eq!(driver.actuator().calls(), vec![3072 * MIB]);
        assert_eq!(driver.actuator().requested().await.unwrap(), 3072 * MIB);

        let Event::Size { size, .. } = driver.changed().await.unwrap() else {
            panic!("the driver reported something other than a size");
        };

        assert_eq!(size, 3072 * MIB);
    }

    #[tokio::test]
    async fn a_driver_ignores_actuations_meant_for_another_guest() {
        let driver = driver(MockActuator::new(BOOT));

        driver
            .apply(&[
                Action::SetRequested {
                    guest: id(TWO),
                    bytes: 1024 * MIB,
                },
                Action::Close { peer: vsock(ONE) },
            ])
            .await
            .unwrap();

        assert_eq!(driver.actuator().calls(), Vec::<u64>::new());
    }

    #[tokio::test]
    async fn a_device_that_only_goes_part_of_the_way_says_so() {
        let driver = driver(MockActuator::new(BOOT).completion(Completion::Partial(2560 * MIB)));

        driver
            .apply(&[Action::SetRequested {
                guest: id(ONE),
                bytes: 3072 * MIB,
            }])
            .await
            .unwrap();

        let Event::Size { size, .. } = driver.changed().await.unwrap() else {
            panic!("the driver reported something other than a size");
        };

        assert_eq!(size, 2560 * MIB);
        assert_eq!(driver.actuator().requested().await.unwrap(), 3072 * MIB);
    }

    #[tokio::test]
    async fn a_device_that_will_not_move_keeps_the_request_standing() {
        let driver = driver(MockActuator::new(BOOT).completion(Completion::Stuck));

        driver
            .apply(&[Action::SetRequested {
                guest: id(ONE),
                bytes: 1536 * MIB,
            }])
            .await
            .unwrap();

        assert_eq!(driver.actuator().requested().await.unwrap(), 1536 * MIB);
        assert_eq!(driver.actuator().reading().await.unwrap().size, BOOT);
    }

    #[tokio::test]
    async fn a_refused_actuation_abandons_the_rest_of_the_batch() {
        let driver = driver(MockActuator::new(BOOT).refusing());

        let error = driver
            .apply(&[
                Action::SetRequested {
                    guest: id(ONE),
                    bytes: 3072 * MIB,
                },
                Action::SetRequested {
                    guest: id(ONE),
                    bytes: 2560 * MIB,
                },
            ])
            .await
            .expect_err("the device refuses everything");

        assert!(
            error.to_string().contains("3221225472"),
            "unexpected error: {error}"
        );
        assert_eq!(driver.actuator().calls(), Vec::<u64>::new());
        assert_eq!(driver.actuator().reading().await.unwrap().size, BOOT);
    }
}
