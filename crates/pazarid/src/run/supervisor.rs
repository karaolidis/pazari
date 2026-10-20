use std::{future::Future, ops::ControlFlow};

use pazari_types::{Backoff, Clock};
use tokio::{
    sync::{mpsc, watch},
    time::sleep,
};
use tracing::{debug, info, warn};

use crate::{
    broker::{Event, GuestId},
    config::Qmp,
    run::metrics::DeviceMetrics,
    runtime::{Actuator, Driver, QmpActuator, ResizeError, RuntimeError},
};

pub trait Connect: Send + Sync {
    type Device: Actuator;

    fn connect(&self) -> impl Future<Output = Result<Self::Device, RuntimeError>> + Send;
}

impl Connect for Qmp {
    type Device = QmpActuator;

    async fn connect(&self) -> Result<QmpActuator, RuntimeError> {
        QmpActuator::connect(
            self.path.clone(),
            self.device.clone(),
            self.mode,
            self.deadline,
        )
        .await
    }
}

#[derive(Debug)]
enum Step {
    Sized(Result<Event, RuntimeError>),

    Target,

    Gone,
}

/// Whether a device has answered before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seeded {
    Never,
    Before,
}

#[derive(Debug)]
pub struct Supervisor<C> {
    guest: GuestId,
    name: String,
    connect: C,
    clock: Clock,
    sizes: mpsc::Sender<Event>,
    targets: watch::Receiver<Option<u64>>,
    backoff: Backoff,
    metrics: DeviceMetrics,
    seeded: Seeded,
}

impl<C: Connect> Supervisor<C> {
    #[must_use]
    pub fn new(
        guest: GuestId,
        name: impl Into<String>,
        connect: C,
        clock: Clock,
        sizes: mpsc::Sender<Event>,
        targets: watch::Receiver<Option<u64>>,
        backoff: Backoff,
        metrics: DeviceMetrics,
    ) -> Self {
        Self {
            guest,
            name: name.into(),
            connect,
            clock,
            sizes,
            targets,
            backoff,
            metrics,
            seeded: Seeded::Never,
        }
    }

    pub async fn run(mut self) {
        while self.attach().await.is_continue() {}
    }

    async fn attach(&mut self) -> ControlFlow<()> {
        let device = match self.connect.connect().await {
            Ok(device) => device,
            Err(error) => return self.retry("failed to open the memory device", &error).await,
        };

        let driver = Driver::new(self.guest, device, self.clock);

        match driver.seed().await {
            Ok(event) => {
                if self.sizes.send(event).await.is_err() {
                    return ControlFlow::Break(());
                }
            }
            Err(error) => {
                self.metrics.seed_failed();

                return self.retry("failed to read the memory size", &error).await;
            }
        }

        if self.seeded == Seeded::Before {
            self.metrics.reconnected();
        }

        self.seeded = Seeded::Before;
        self.backoff.reset();

        info!(guest = %self.name, "memory device responded");

        self.serve(&driver).await
    }

    async fn serve(&mut self, driver: &Driver<C::Device>) -> ControlFlow<()> {
        loop {
            let step = tokio::select! {
                changed = driver.changed() => Step::Sized(changed),
                target = self.targets.changed() => {
                    if target.is_ok() { Step::Target } else { Step::Gone }
                }
            };

            match step {
                Step::Sized(Ok(event)) => {
                    if self.sizes.send(event).await.is_err() {
                        return ControlFlow::Break(());
                    }
                }
                Step::Sized(Err(error)) => {
                    return self.retry("memory device stopped responding", &error).await;
                }
                Step::Target => {
                    let Some(bytes) = *self.targets.borrow_and_update() else {
                        continue;
                    };

                    debug!(guest = %self.name, bytes, "resizing the memory device");

                    match driver.actuator().set_requested(bytes).await {
                        Ok(()) => {}
                        Err(RuntimeError::Resize(error)) => {
                            if matches!(error, ResizeError::Refused { .. }) {
                                self.metrics.refused();
                            }

                            warn!(
                                guest = %self.name,
                                bytes,
                                %error,
                                "resize failed; deferring to reconciliation",
                            );
                        }
                        Err(error) => {
                            // The reopened device is asked again.
                            self.targets.mark_changed();

                            return self.retry("memory device stopped responding", &error).await;
                        }
                    }
                }
                Step::Gone => return ControlFlow::Break(()),
            }
        }
    }

    async fn retry(&mut self, what: &'static str, error: &RuntimeError) -> ControlFlow<()> {
        let delay = self.backoff.delay();

        warn!(guest = %self.name, %error, ?delay, "{what}; retrying");

        sleep(delay).await;

        ControlFlow::Continue(())
    }
}

#[cfg(test)]
mod tests {
    use std::{future, time::Duration};

    use pazari_metrics::{Encoded, Registry};
    use tokio::{
        task,
        time::{Instant, timeout},
    };

    use super::*;
    use crate::{
        broker::testing::{BOOT, GIB, MIB, ONE, id},
        config::{RECONNECT_MAX, RECONNECT_MIN},
        run::metrics::Observer,
        runtime::testing::MockActuator,
    };

    const GROWN: u64 = 3072 * MIB;

    const PATIENCE: Duration = Duration::from_secs(5);

    const DEPTH: usize = 8;

    impl Connect for MockActuator {
        type Device = Self;

        fn connect(&self) -> impl Future<Output = Result<Self, RuntimeError>> + Send {
            future::ready(self.open())
        }
    }

    struct Watched {
        device: MockActuator,
        sizes: mpsc::Receiver<Event>,
        target: watch::Sender<Option<u64>>,
        supervisor: task::JoinHandle<()>,
        metrics: Registry,
    }

    impl Watched {
        fn start(device: MockActuator) -> Self {
            let (sizes, queue) = mpsc::channel(DEPTH);
            let (target, targets) = watch::channel(None);
            let (observer, metrics) = Observer::new();

            let supervisor = tokio::spawn(
                Supervisor::new(
                    id(ONE),
                    "one",
                    device.clone(),
                    Clock::start(),
                    sizes,
                    targets,
                    Backoff::new(RECONNECT_MIN, RECONNECT_MAX),
                    observer.device("one"),
                )
                .run(),
            );

            Self {
                device,
                sizes: queue,
                target,
                supervisor,
                metrics,
            }
        }

        fn counted(&self, series: &str) -> bool {
            self.metrics
                .encoded()
                .expect("a registry encodes")
                .contains(series)
        }

        async fn sized(&mut self) -> u64 {
            let event = timeout(PATIENCE, self.sizes.recv())
                .await
                .expect("the supervisor reported a size in time")
                .expect("the supervisor is still running");

            let Event::Size { guest, size, .. } = event else {
                panic!("the supervisor reported {event:?}");
            };

            assert_eq!(guest, id(ONE), "a size meant for another guest");

            size
        }

        async fn settles(&self, what: &str, ready: impl Fn(&MockActuator) -> bool) {
            let waiting = async {
                while !ready(&self.device) {
                    task::yield_now().await;
                }
            };

            assert!(
                timeout(PATIENCE, waiting).await.is_ok(),
                "timed out waiting for {what}"
            );
        }
    }

    impl Drop for Watched {
        fn drop(&mut self) {
            self.supervisor.abort();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_device_that_will_not_open_is_tried_again_on_a_climbing_delay() {
        let started = Instant::now();
        let mut watched = Watched::start(MockActuator::new(BOOT).refusing_to_open(2));

        assert_eq!(watched.sized().await, BOOT);
        assert_eq!(watched.device.opens(), 3);
        assert_eq!(
            Instant::now().duration_since(started),
            RECONNECT_MIN + RECONNECT_MIN * 2,
            "the delays between attempts did not double",
        );
        assert!(
            watched.counted("pazarid_device_reconnects_total{guest=\"one\"} 0"),
            "an attempt that never opened the device was counted as a reconnect",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_device_that_opens_but_will_not_be_read_climbs_the_delay_too() {
        let started = Instant::now();
        let mut watched = Watched::start(MockActuator::new(BOOT).refusing_to_be_read(2));

        assert_eq!(watched.sized().await, BOOT);
        assert_eq!(watched.device.opens(), 3);
        assert_eq!(
            Instant::now().duration_since(started),
            RECONNECT_MIN + RECONNECT_MIN * 2,
            "a device that opens and says nothing was hammered at the floor",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_device_that_dies_is_reopened_and_reseeded() {
        let mut watched = Watched::start(MockActuator::new(BOOT));

        assert_eq!(watched.sized().await, BOOT);

        watched.device.plug(GROWN);

        assert_eq!(watched.sized().await, GROWN);

        watched.device.kill();
        watched.device.rebooted(BOOT);

        assert_eq!(
            watched.sized().await,
            BOOT,
            "the reopened device never reseeded the broker",
        );
        assert_eq!(watched.device.opens(), 2);
        assert!(watched.counted("pazarid_device_reconnects_total{guest=\"one\"} 1"));

        watched.device.plug(GROWN);

        assert_eq!(watched.sized().await, GROWN);
    }

    #[tokio::test]
    async fn a_target_reaches_the_device_it_names() {
        let mut watched = Watched::start(MockActuator::new(BOOT));

        assert_eq!(watched.sized().await, BOOT);

        watched.target.send_replace(Some(GROWN));
        watched
            .settles("the resize", |device| !device.calls().is_empty())
            .await;

        assert_eq!(watched.device.calls(), vec![GROWN]);
    }

    #[tokio::test]
    async fn a_size_the_device_refuses_leaves_the_guest_served() {
        let mut watched = Watched::start(MockActuator::new(BOOT).refusing());

        assert_eq!(watched.sized().await, BOOT);

        watched.target.send_replace(Some(GROWN));
        watched
            .settles("the refused resize", |device| !device.calls().is_empty())
            .await;

        watched.device.plug(GIB);

        assert_eq!(
            watched.sized().await,
            GIB,
            "a refused resize took the guest's device down with it",
        );
        assert_eq!(watched.device.opens(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_device_that_misbehaves_is_counted_against_its_guest() {
        let mut watched = Watched::start(MockActuator::new(BOOT).refusing_to_be_read(1).refusing());

        assert_eq!(watched.sized().await, BOOT);

        assert!(watched.counted("pazarid_device_seed_failures_total{guest=\"one\"} 1"));
        assert!(watched.counted("pazarid_device_reconnects_total{guest=\"one\"} 0"));

        watched.target.send_replace(Some(GROWN));
        watched
            .settles("the refused resize", |_| {
                watched.counted("pazarid_device_refused_actuations_total{guest=\"one\"} 1")
            })
            .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_resize_lost_on_the_wire_reopens_the_device_and_is_asked_again() {
        let mut watched = Watched::start(MockActuator::new(BOOT).failing_writes(1));

        assert_eq!(watched.sized().await, BOOT);

        watched.target.send_replace(Some(GROWN));

        assert_eq!(watched.sized().await, BOOT, "the device was not reseeded");
        assert_eq!(watched.sized().await, GROWN);
        assert_eq!(watched.device.calls(), vec![GROWN, GROWN]);
        assert_eq!(watched.device.opens(), 2);
        assert!(watched.counted("pazarid_device_refused_actuations_total{guest=\"one\"} 0"));
        assert!(watched.counted("pazarid_device_reconnects_total{guest=\"one\"} 1"));
    }

    #[tokio::test]
    async fn a_supervisor_stops_once_the_daemon_has() {
        let device = MockActuator::new(BOOT);
        let (sizes, queue) = mpsc::channel(DEPTH);
        let (target, targets) = watch::channel(None);

        let supervisor = tokio::spawn(
            Supervisor::new(
                id(ONE),
                "one",
                device,
                Clock::start(),
                sizes,
                targets,
                Backoff::new(RECONNECT_MIN, RECONNECT_MAX),
                DeviceMetrics::default(),
            )
            .run(),
        );

        drop(queue);
        drop(target);

        assert!(
            timeout(PATIENCE, supervisor)
                .await
                .expect("the supervisor stopped in time")
                .is_ok()
        );
    }
}
