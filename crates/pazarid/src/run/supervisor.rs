use std::future::Future;

use pazari_types::{Backoff, Clock};
use tokio::{
    sync::{mpsc, watch},
    time::sleep,
};
use tracing::{info, warn};

use crate::{
    broker::{Event, GuestId},
    config::Qmp,
    runtime::{Actuator, Driver, QmpActuator, RuntimeError},
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

#[derive(Debug)]
pub struct Supervisor<C> {
    guest: GuestId,
    name: String,
    connect: C,
    clock: Clock,
    sizes: mpsc::Sender<Event>,
    targets: watch::Receiver<Option<u64>>,
    backoff: Backoff,
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
    ) -> Self {
        Self {
            guest,
            name: name.into(),
            connect,
            clock,
            sizes,
            targets,
            backoff,
        }
    }

    pub async fn run(mut self) {
        while self.attach().await {}
    }

    async fn attach(&mut self) -> bool {
        let device = match self.connect.connect().await {
            Ok(device) => device,
            Err(error) => {
                return self
                    .retry("failed to open the guest's memory device", &error)
                    .await;
            }
        };

        let driver = Driver::new(self.guest, device, self.clock);

        match driver.seed().await {
            Ok(event) => {
                if self.sizes.send(event).await.is_err() {
                    return false;
                }
            }
            Err(error) => {
                return self
                    .retry("failed to read the guest's memory size", &error)
                    .await;
            }
        }

        self.backoff.reset();

        info!(guest = %self.name, "the guest's memory device answered");

        self.serve(&driver).await
    }

    async fn serve(&mut self, driver: &Driver<C::Device>) -> bool {
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
                        return false;
                    }
                }
                Step::Sized(Err(error)) => {
                    return self
                        .retry("the guest's memory device stopped answering", &error)
                        .await;
                }
                Step::Target => {
                    let Some(bytes) = *self.targets.borrow_and_update() else {
                        continue;
                    };

                    if let Err(error) = driver.actuator().set_requested(bytes).await {
                        warn!(
                            guest = %self.name,
                            bytes,
                            %error,
                            "failed to resize the guest; leaving it to reconciliation",
                        );
                    }
                }
                Step::Gone => return false,
            }
        }
    }

    async fn retry(&mut self, what: &'static str, error: &RuntimeError) -> bool {
        let delay = self.backoff.delay();

        warn!(guest = %self.name, %error, ?delay, "{what}; trying again");

        sleep(delay).await;

        true
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future,
        sync::{Arc, Mutex, MutexGuard},
        time::Duration,
    };

    use tokio::{
        task,
        time::{Instant, timeout},
    };

    use super::*;
    use crate::broker::testing::{BOOT, GIB, MIB, ONE, id};

    const GROWN: u64 = 3072 * MIB;

    const PATIENCE: Duration = Duration::from_secs(5);

    const RETRY_MIN: Duration = Duration::from_millis(500);

    const RETRY_MAX: Duration = Duration::from_secs(30);

    const DEPTH: usize = 8;

    #[derive(Debug, Default)]
    struct State {
        opens: usize,
        refusals: usize,
        blindness: usize,
        size: u64,
        calls: Vec<u64>,
        refuse: bool,
    }

    #[derive(Debug, Clone)]
    struct Socket {
        state: Arc<Mutex<State>>,
        changes: mpsc::UnboundedSender<Result<u64, ()>>,
        pending: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<Result<u64, ()>>>>,
    }

    impl Socket {
        fn new(size: u64) -> Self {
            let (changes, pending) = mpsc::unbounded_channel();

            Self {
                state: Arc::new(Mutex::new(State {
                    size,
                    ..State::default()
                })),
                changes,
                pending: Arc::new(tokio::sync::Mutex::new(pending)),
            }
        }

        fn refusing_to_open(self, refusals: usize) -> Self {
            self.locked().refusals = refusals;
            self
        }

        fn refusing_to_be_read(self, blindness: usize) -> Self {
            self.locked().blindness = blindness;
            self
        }

        fn refusing_sizes(self) -> Self {
            self.locked().refuse = true;
            self
        }

        fn locked(&self) -> MutexGuard<'_, State> {
            self.state.lock().expect("no test poisons the socket")
        }

        fn plug(&self, size: u64) {
            self.changes
                .send(Ok(size))
                .expect("the socket holds its own receiver");
        }

        fn kill(&self) {
            self.changes
                .send(Err(()))
                .expect("the socket holds its own receiver");
        }

        fn rebooted(&self, size: u64) {
            self.locked().size = size;
        }

        fn opens(&self) -> usize {
            self.locked().opens
        }

        fn calls(&self) -> Vec<u64> {
            self.locked().calls.clone()
        }
    }

    fn failed(reason: &str) -> RuntimeError {
        RuntimeError::Device {
            bytes: 0,
            reason: reason.to_owned(),
        }
    }

    impl Connect for Socket {
        type Device = Self;

        fn connect(&self) -> impl Future<Output = Result<Self, RuntimeError>> + Send {
            let mut state = self.locked();

            state.opens = state.opens.saturating_add(1);

            let refused = state.refusals > 0;

            if refused {
                state.refusals -= 1;
            }

            drop(state);

            future::ready(if refused {
                Err(failed("the socket is not there"))
            } else {
                Ok(self.clone())
            })
        }
    }

    impl Actuator for Socket {
        fn size(&self) -> impl Future<Output = Result<u64, RuntimeError>> + Send {
            let mut state = self.locked();

            let blind = state.blindness > 0;

            if blind {
                state.blindness -= 1;
            }

            let size = state.size;
            drop(state);

            future::ready(if blind {
                Err(failed("the socket will not say what size it is"))
            } else {
                Ok(size)
            })
        }

        fn requested(&self) -> impl Future<Output = Result<u64, RuntimeError>> + Send {
            future::ready(Ok(self.locked().size))
        }

        fn set_requested(
            &self,
            bytes: u64,
        ) -> impl Future<Output = Result<(), RuntimeError>> + Send {
            let mut state = self.locked();

            state.calls.push(bytes);
            let refuse = state.refuse;
            drop(state);

            future::ready(if refuse {
                Err(failed("the device refuses every size"))
            } else {
                Ok(())
            })
        }

        async fn changed(&self) -> Result<u64, RuntimeError> {
            let mut pending = self.pending.lock().await;

            match pending.recv().await {
                Some(Ok(size)) => {
                    self.locked().size = size;

                    Ok(size)
                }
                Some(Err(())) => Err(failed("the socket closed")),
                None => future::pending().await,
            }
        }
    }

    struct Watched {
        socket: Socket,
        sizes: mpsc::Receiver<Event>,
        target: watch::Sender<Option<u64>>,
        supervisor: task::JoinHandle<()>,
    }

    impl Watched {
        fn start(socket: Socket) -> Self {
            let (sizes, queue) = mpsc::channel(DEPTH);
            let (target, targets) = watch::channel(None);

            let supervisor = tokio::spawn(
                Supervisor::new(
                    id(ONE),
                    "one",
                    socket.clone(),
                    Clock::start(),
                    sizes,
                    targets,
                    Backoff::new(RETRY_MIN, RETRY_MAX),
                )
                .run(),
            );

            Self {
                socket,
                sizes: queue,
                target,
                supervisor,
            }
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

        async fn settles(&self, what: &str, ready: impl Fn(&Socket) -> bool) {
            let waiting = async {
                while !ready(&self.socket) {
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
        let mut watched = Watched::start(Socket::new(BOOT).refusing_to_open(2));

        assert_eq!(watched.sized().await, BOOT);
        assert_eq!(watched.socket.opens(), 3);
        assert_eq!(
            Instant::now().duration_since(started),
            RETRY_MIN + RETRY_MIN * 2,
            "the delays between attempts did not double",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_device_that_opens_but_will_not_be_read_climbs_the_delay_too() {
        let started = Instant::now();
        let mut watched = Watched::start(Socket::new(BOOT).refusing_to_be_read(2));

        assert_eq!(watched.sized().await, BOOT);
        assert_eq!(watched.socket.opens(), 3);
        assert_eq!(
            Instant::now().duration_since(started),
            RETRY_MIN + RETRY_MIN * 2,
            "a device that opens and says nothing was hammered at the floor",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_device_that_dies_is_reopened_and_reseeded() {
        let mut watched = Watched::start(Socket::new(BOOT));

        assert_eq!(watched.sized().await, BOOT);

        watched.socket.plug(GROWN);

        assert_eq!(watched.sized().await, GROWN);

        watched.socket.kill();
        watched.socket.rebooted(BOOT);

        assert_eq!(
            watched.sized().await,
            BOOT,
            "the reopened device never reseeded the broker",
        );
        assert_eq!(watched.socket.opens(), 2);

        watched.socket.plug(GROWN);

        assert_eq!(watched.sized().await, GROWN);
    }

    #[tokio::test]
    async fn a_target_reaches_the_device_it_names() {
        let mut watched = Watched::start(Socket::new(BOOT));

        assert_eq!(watched.sized().await, BOOT);

        watched.target.send_replace(Some(GROWN));
        watched
            .settles("the resize", |socket| !socket.calls().is_empty())
            .await;

        assert_eq!(watched.socket.calls(), vec![GROWN]);
    }

    #[tokio::test]
    async fn a_size_the_device_refuses_leaves_the_guest_served() {
        let mut watched = Watched::start(Socket::new(BOOT).refusing_sizes());

        assert_eq!(watched.sized().await, BOOT);

        watched.target.send_replace(Some(GROWN));
        watched
            .settles("the refused resize", |socket| !socket.calls().is_empty())
            .await;

        watched.socket.plug(GIB);

        assert_eq!(
            watched.sized().await,
            GIB,
            "a refused resize took the guest's device down with it",
        );
        assert_eq!(watched.socket.opens(), 1);
    }

    #[tokio::test]
    async fn a_supervisor_stops_once_the_daemon_has() {
        let socket = Socket::new(BOOT);
        let (sizes, queue) = mpsc::channel(DEPTH);
        let (target, targets) = watch::channel(None);

        let supervisor = tokio::spawn(
            Supervisor::new(
                id(ONE),
                "one",
                socket,
                Clock::start(),
                sizes,
                targets,
                Backoff::new(RETRY_MIN, RETRY_MAX),
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
