use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use pazari_types::Interval;
use serde_json::{Value, json};
use tokio::{
    sync::watch,
    time::{sleep, timeout},
};

use crate::runtime::{
    actuator::{Actuator, Reading},
    error::{ResizeError, RuntimeError},
    qmp::{
        Endpoint, Held, MemoryDevice, MemorySummary, Operation, QmpCommand, QmpError, Session,
        qom_path,
    },
};

const SIZE: &str = "size";
const REQUESTED_SIZE: &str = "requested-size";
const VIRTIO_MEM: &str = "virtio-mem";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Held,

    PerOperation { poll: Interval },
}

/// How the actuator reaches the device.
///
/// A connection that fails stays failed; reopening it is the caller's to do.
#[derive(Debug)]
enum Link {
    Held(Arc<Held>),

    PerOperation {
        poll: Interval,

        /// The size a resize in flight is heading to, while one is.
        resizing: watch::Sender<Option<u64>>,
    },
}

/// One guest's virtio-mem device over QMP.
#[derive(Debug)]
pub struct QmpActuator {
    path: PathBuf,
    device: String,
    qom: String,
    deadline: Interval,
    boot: u64,
    last: AtomicU64,
    link: Link,
}

impl QmpActuator {
    pub async fn connect(
        path: impl Into<PathBuf>,
        device: impl Into<String>,
        mode: Mode,
        deadline: Interval,
    ) -> Result<Self, RuntimeError> {
        let path = path.into();
        let device = device.into();
        let qom = qom_path(&device);

        let mut session =
            deadlined(Session::open(&path), deadline, Operation::Handshake, &path).await?;

        let boot = verify(&mut session, &device, deadline).await?;
        let size = boot.saturating_add(read(&mut session, &qom, SIZE, deadline).await?);

        let link = match mode {
            Mode::Held => Link::Held(Arc::new(Held::take(session, &device))),
            Mode::PerOperation { poll } => {
                let requested =
                    boot.saturating_add(read(&mut session, &qom, REQUESTED_SIZE, deadline).await?);

                // A resize asked for before this connection may still be on its way.
                let converging = (requested != size).then_some(requested);

                Link::PerOperation {
                    poll,
                    resizing: watch::Sender::new(converging),
                }
            }
        };

        Ok(Self {
            path,
            device,
            qom,
            deadline,
            boot,
            last: AtomicU64::new(size),
            link,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn device(&self) -> &str {
        &self.device
    }

    #[must_use]
    pub const fn boot(&self) -> u64 {
        self.boot
    }

    async fn wire(&self) -> Result<Wire, QmpError> {
        match &self.link {
            Link::Held(held) => Ok(Wire::Held(Arc::clone(held))),
            Link::PerOperation { .. } => Ok(Wire::Fresh(
                deadlined(
                    Session::open(&self.path),
                    self.deadline,
                    Operation::Handshake,
                    &self.path,
                )
                .await?,
            )),
        }
    }

    async fn property(&self, property: &'static str) -> Result<u64, QmpError> {
        read(&mut self.wire().await?, &self.qom, property, self.deadline).await
    }

    fn report(&self, reading: Reading) -> Option<Reading> {
        (self.last.swap(reading.size, Ordering::Relaxed) != reading.size).then_some(reading)
    }

    async fn awaited(&self, mut held: &Held) -> Result<Reading, RuntimeError> {
        loop {
            let mut sizes = held.watch();

            let sampled = read(&mut held, &self.qom, SIZE, self.deadline).await?;

            if let Some(reading) = self.report(Reading::plugged(self.boot, sampled)) {
                return Ok(reading);
            }

            let raised = held.changed(&mut sizes).await?;

            if let Some(reading) = self.report(Reading::plugged(self.boot, raised)) {
                return Ok(reading);
            }
        }
    }

    async fn resampled(
        &self,
        poll: Interval,
        resizing: &watch::Sender<Option<u64>>,
    ) -> Result<Reading, RuntimeError> {
        let mut armed = resizing.subscribe();

        loop {
            let target = armed
                .wait_for(Option::is_some)
                .await
                .ok()
                .and_then(|target| *target)
                .expect("the actuator holds the sender of an armed resize");

            let reading = Reading::plugged(self.boot, self.property(SIZE).await?);
            let landed = reading.size == target;

            if landed {
                resizing.send_if_modified(|resizing| {
                    let current = *resizing == Some(target);

                    if current {
                        *resizing = None;
                    }

                    current
                });
            }

            if let Some(reading) = self.report(reading) {
                return Ok(reading);
            }

            if !landed {
                sleep(poll.get()).await;
            }
        }
    }
}

impl Actuator for QmpActuator {
    async fn reading(&self) -> Result<Reading, RuntimeError> {
        let hotplugged = self.property(SIZE).await?;

        Ok(Reading::plugged(self.boot, hotplugged))
    }

    async fn requested(&self) -> Result<u64, RuntimeError> {
        let requested = self.property(REQUESTED_SIZE).await?;

        Ok(self.boot.saturating_add(requested))
    }

    async fn set_requested(&self, bytes: u64) -> Result<(), RuntimeError> {
        let boot = self.boot;

        if bytes < boot {
            return Err(ResizeError::BelowBoot { bytes, boot }.into());
        }

        let mut wire = self.wire().await?;

        let arguments = json!({
            "path": self.qom,
            "property": REQUESTED_SIZE,
            "value": bytes.saturating_sub(boot),
        });

        let set = deadlined(
            wire.execute(QmpCommand::QomSet, arguments),
            self.deadline,
            Operation::Command(QmpCommand::QomSet),
            &self.path,
        )
        .await;

        match set {
            Ok(_) => {}
            Err(QmpError::Refused { reason, .. }) => {
                return Err(ResizeError::Refused { bytes, reason }.into());
            }
            Err(error) => return Err(error.into()),
        }

        if let Link::PerOperation { resizing, .. } = &self.link {
            resizing.send_replace(Some(bytes));
        }

        Ok(())
    }

    async fn changed(&self) -> Result<Reading, RuntimeError> {
        match &self.link {
            Link::Held(held) => self.awaited(held).await,
            Link::PerOperation { poll, resizing, .. } => self.resampled(*poll, resizing).await,
        }
    }
}

async fn deadlined<T>(
    operation: impl Future<Output = Result<T, QmpError>>,
    deadline: Interval,
    during: Operation,
    path: &Path,
) -> Result<T, QmpError> {
    timeout(deadline.get(), operation)
        .await
        .map_err(|_| QmpError::Timeout {
            during,
            path: path.to_path_buf(),
            deadline,
        })?
}

/// Runs a command that takes no arguments on a fresh session.
async fn query(
    session: &mut Session,
    command: QmpCommand,
    deadline: Interval,
) -> Result<Value, QmpError> {
    let path = session.path().to_path_buf();

    deadlined(
        session.execute(command, Value::Null),
        deadline,
        Operation::Command(command),
        &path,
    )
    .await
}

async fn verify(session: &mut Session, device: &str, deadline: Interval) -> Result<u64, QmpError> {
    let path = session.path().to_path_buf();

    let command = QmpCommand::QueryMemoryDevices;
    let value = query(session, command, deadline).await?;

    let devices: Vec<MemoryDevice> =
        serde_json::from_value(value).map_err(|source| QmpError::Protocol {
            path: path.clone(),
            reason: format!("{command} answered with {source}"),
        })?;

    let found = devices
        .into_iter()
        .find(|entry| entry.data.id.as_deref() == Some(device))
        .ok_or_else(|| QmpError::NoDevice {
            device: device.to_owned(),
        })?;

    if found.kind != VIRTIO_MEM {
        return Err(QmpError::NotVirtioMem {
            device: device.to_owned(),
            kind: found.kind,
        });
    }

    let command = QmpCommand::QueryMemorySizeSummary;
    let value = query(session, command, deadline).await?;

    let summary: MemorySummary =
        serde_json::from_value(value).map_err(|source| QmpError::Protocol {
            path,
            reason: format!("{command} answered with {source}"),
        })?;

    Ok(summary.base_memory)
}

#[derive(Debug)]
enum Wire {
    Held(Arc<Held>),
    Fresh(Session),
}

impl Endpoint for Wire {
    fn path(&self) -> &Path {
        match self {
            Self::Held(held) => held.path(),
            Self::Fresh(session) => session.path(),
        }
    }

    async fn execute(&mut self, command: QmpCommand, arguments: Value) -> Result<Value, QmpError> {
        match self {
            Self::Held(held) => held.execute(command, arguments).await,
            Self::Fresh(session) => session.execute(command, arguments).await,
        }
    }
}

/// Reads a `u64` property off the device's QOM path.
async fn read(
    endpoint: &mut impl Endpoint,
    qom: &str,
    property: &'static str,
    deadline: Interval,
) -> Result<u64, QmpError> {
    let path = endpoint.path().to_path_buf();

    let value = deadlined(
        endpoint.execute(
            QmpCommand::QomGet,
            json!({ "path": qom, "property": property }),
        ),
        deadline,
        Operation::Command(QmpCommand::QomGet),
        &path,
    )
    .await?;

    value.as_u64().ok_or_else(|| QmpError::Protocol {
        path,
        reason: format!("{property} came back as {value}"),
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::time::timeout;

    use super::*;
    use crate::{
        broker::testing::{BOOT, MIB},
        runtime::{
            qmp::{
                MAX_LINE_BYTES,
                QmpCommand::{
                    Capabilities, QomGet, QomSet, QueryMemoryDevices, QueryMemorySizeSummary,
                },
            },
            testing::qmp::{FakeQmp, Step, devices, resized, resized_anonymously, shared, startup},
        },
    };

    const DEVICE: &str = "vm0";
    const OTHER: &str = "vm1";

    const QUIET: Duration = Duration::from_millis(50);

    const STARTUP: [QmpCommand; 4] = [
        Capabilities,
        QueryMemoryDevices,
        QueryMemorySizeSummary,
        QomGet,
    ];

    fn deadline() -> Interval {
        Interval::try_new(Duration::from_secs(5)).expect("five seconds")
    }

    fn impatient() -> Interval {
        Interval::try_new(Duration::from_millis(100)).expect("a tenth of a second")
    }

    fn polling() -> Mode {
        Mode::PerOperation {
            poll: Interval::try_new(Duration::from_millis(5)).expect("five milliseconds"),
        }
    }

    fn stopped() -> String {
        json!({
            "timestamp": { "seconds": 1_700_000_000, "microseconds": 0 },
            "event": "STOP",
        })
        .to_string()
    }

    async fn open(server: &FakeQmp, mode: Mode) -> QmpActuator {
        QmpActuator::connect(server.path(), DEVICE, mode, deadline())
            .await
            .expect("the device is there")
    }

    async fn held(steps: Vec<Step>) -> (FakeQmp, QmpActuator) {
        let mut script = startup(DEVICE, BOOT, 0);
        script.extend(steps);

        let server = FakeQmp::start([script]);
        let actuator = open(&server, Mode::Held).await;

        (server, actuator)
    }

    async fn settled(server: &FakeQmp, commands: usize) {
        while server.commands().len() < commands {
            sleep(Duration::from_millis(1)).await;
        }
    }

    #[tokio::test]
    async fn the_handshake_comes_before_the_first_command() {
        let (server, actuator) = held(Vec::new()).await;

        assert_eq!(server.commands(), STARTUP);
        assert_eq!(server.connections(), 1);
        assert_eq!(actuator.path(), server.path());
        assert_eq!(actuator.device(), DEVICE);
        assert_eq!(actuator.boot(), BOOT);
    }

    #[tokio::test]
    async fn the_sizes_it_reports_stand_on_the_boot_memory() {
        let (server, actuator) = held(vec![
            Step::returns(QomGet, json!(1024 * MIB)),
            Step::returns(QomGet, json!(1536 * MIB)),
        ])
        .await;

        assert_eq!(actuator.reading().await.unwrap().size, BOOT + 1024 * MIB);
        assert_eq!(actuator.requested().await.unwrap(), BOOT + 1536 * MIB);

        assert_eq!(
            server.arguments(QomGet),
            [
                json!({ "path": qom_path(DEVICE), "property": "size" }),
                json!({ "path": qom_path(DEVICE), "property": "size" }),
                json!({ "path": qom_path(DEVICE), "property": "requested-size" }),
            ]
        );
        assert_eq!(server.connections(), 1);
    }

    #[tokio::test]
    async fn an_ask_reaches_the_device_without_the_boot_memory() {
        let (server, actuator) = held(vec![Step::returns(QomSet, json!({}))]).await;

        actuator.set_requested(BOOT + 512 * MIB).await.unwrap();

        assert_eq!(
            server.arguments(QomSet),
            [json!({
                "path": qom_path(DEVICE),
                "property": "requested-size",
                "value": 512 * MIB,
            })]
        );
    }

    #[tokio::test]
    async fn a_size_the_device_will_not_take_comes_back_as_a_refusal() {
        let (_server, actuator) = held(vec![Step::refuses(
            QomSet,
            "Property 'requested-size' doesn't take value '1048577'",
        )])
        .await;

        let error = actuator
            .set_requested(BOOT + MIB + 1)
            .await
            .expect_err("the size is not block aligned");

        assert_eq!(
            error.to_string(),
            format!(
                "the memory device refused a size of {}: \
                 Property 'requested-size' doesn't take value '1048577'",
                BOOT + MIB + 1
            )
        );
    }

    #[tokio::test]
    async fn a_refused_handshake_is_not_the_device_refusing() {
        let server = FakeQmp::start([
            shared(DEVICE, BOOT, 0, 0),
            vec![Step::refuses(Capabilities, "this socket is read-only")],
        ]);

        let actuator = open(&server, polling()).await;

        let error = actuator
            .set_requested(BOOT + 512 * MIB)
            .await
            .expect_err("the handshake is refused");

        assert_eq!(
            error.to_string(),
            "qemu refused qmp_capabilities: this socket is read-only"
        );
    }

    #[tokio::test]
    async fn an_ask_under_the_boot_memory_never_reaches_the_wire() {
        let (server, actuator) = held(Vec::new()).await;

        let error = actuator
            .set_requested(BOOT - MIB)
            .await
            .expect_err("the guest cannot give back its boot memory");

        assert_eq!(
            error.to_string(),
            format!(
                "asked the device for {}, under the guest's boot memory of {BOOT}",
                BOOT - MIB
            )
        );
        assert_eq!(server.commands(), STARTUP);
    }

    #[tokio::test]
    async fn a_device_the_guest_does_not_have_is_refused_at_startup() {
        let server = FakeQmp::start([vec![Step::returns(
            QueryMemoryDevices,
            devices(&[(OTHER, "virtio-mem")]),
        )]]);

        let error = QmpActuator::connect(server.path(), DEVICE, Mode::Held, deadline())
            .await
            .expect_err("the guest carries another device");

        assert_eq!(
            error.to_string(),
            "the guest has no memory device called vm0"
        );
    }

    #[tokio::test]
    async fn a_device_that_cannot_resize_is_refused_at_startup() {
        let server = FakeQmp::start([vec![Step::returns(
            QueryMemoryDevices,
            devices(&[(DEVICE, "pc-dimm")]),
        )]]);

        let error = QmpActuator::connect(server.path(), DEVICE, Mode::Held, deadline())
            .await
            .expect_err("a pc-dimm does not resize");

        assert_eq!(
            error.to_string(),
            "the memory device vm0 is a pc-dimm, not a virtio-mem"
        );
    }

    #[tokio::test]
    async fn a_command_that_is_never_answered_gives_up_at_the_deadline() {
        let server = FakeQmp::start([shared(DEVICE, BOOT, 0, 0), vec![Step::Ignore]]);

        let actuator = QmpActuator::connect(server.path(), DEVICE, polling(), impatient())
            .await
            .expect("the device is there");

        let error = actuator
            .reading()
            .await
            .expect_err("the server never answers");

        assert_eq!(
            error.to_string(),
            format!(
                "qom-get on the QMP socket {} did not finish within 100ms",
                server.path().display()
            )
        );
    }

    #[tokio::test]
    async fn a_command_that_ran_out_of_time_leaves_the_connection_in_frame() {
        let mut script = startup(DEVICE, BOOT, 0);
        script.extend([Step::Ignore, Step::returns(QomGet, json!(256 * MIB))]);

        let server = FakeQmp::start([script]);

        let actuator = QmpActuator::connect(server.path(), DEVICE, Mode::Held, impatient())
            .await
            .expect("the device is there");

        actuator
            .reading()
            .await
            .expect_err("the server never answers");

        assert_eq!(actuator.reading().await.unwrap().size, BOOT + 256 * MIB);
        assert_eq!(server.connections(), 1);
        assert_eq!(server.commands().len(), STARTUP.len() + 2);
    }

    #[tokio::test]
    async fn a_size_change_event_wakes_the_waiter() {
        let (_server, actuator) = held(vec![
            Step::returns(QomSet, json!({})),
            Step::Emit(resized(DEVICE, 1024 * MIB)),
            Step::returns(QomGet, json!(1024 * MIB)),
        ])
        .await;

        actuator.set_requested(BOOT + 1024 * MIB).await.unwrap();

        assert_eq!(actuator.changed().await.unwrap().size, BOOT + 1024 * MIB);
    }

    #[tokio::test]
    async fn an_event_that_only_carries_a_qom_path_still_wakes_the_waiter() {
        let (_server, actuator) = held(vec![
            Step::Emit(resized_anonymously(&qom_path(DEVICE), 2048 * MIB)),
            Step::returns(QomGet, json!(2048 * MIB)),
        ])
        .await;

        assert_eq!(actuator.changed().await.unwrap().size, BOOT + 2048 * MIB);
    }

    #[tokio::test]
    async fn an_event_between_two_calls_is_reported_by_the_second() {
        let (_server, actuator) = held(vec![
            Step::returns(QomGet, json!(256 * MIB)),
            Step::Emit(resized(DEVICE, 512 * MIB)),
            Step::returns(QomGet, json!(512 * MIB)),
        ])
        .await;

        assert_eq!(actuator.changed().await.unwrap().size, BOOT + 256 * MIB);
        assert_eq!(actuator.changed().await.unwrap().size, BOOT + 512 * MIB);
    }

    #[tokio::test]
    async fn nothing_but_this_device_wakes_the_waiter() {
        let (_server, actuator) = held(vec![
            Step::Emit(resized(OTHER, 4096 * MIB)),
            Step::Emit(resized_anonymously(&qom_path(OTHER), 3072 * MIB)),
            Step::Emit(stopped()),
            Step::returns(QomGet, json!(0)),
            Step::returns(QomGet, json!(0)),
        ])
        .await;

        assert_eq!(actuator.reading().await.unwrap().size, BOOT);

        assert!(
            timeout(QUIET, actuator.changed()).await.is_err(),
            "another device's event woke the waiter"
        );
    }

    #[tokio::test]
    async fn a_lost_connection_wakes_the_waiter_and_fails_every_later_command() {
        let mut first = startup(DEVICE, BOOT, 0);
        first.push(Step::Hangup);

        let server = FakeQmp::start([first, startup(DEVICE, BOOT, 0)]);
        let actuator = open(&server, Mode::Held).await;

        let closed = format!("the QMP connection to {} closed", server.path().display());

        let error = actuator.changed().await.expect_err("the server hung up");

        assert_eq!(error.to_string(), closed);

        let error = actuator
            .reading()
            .await
            .expect_err("the actuator reopened a connection on its own");

        assert_eq!(error.to_string(), closed);
        assert_eq!(server.connections(), 1);
    }

    #[tokio::test]
    async fn a_broken_frame_reaches_everyone_waiting() {
        let line = format!(r#"{{"return":"{}","id":1}}"#, "x".repeat(MAX_LINE_BYTES));

        let (server, actuator) = held(vec![
            Step::returns(QomGet, json!(0)),
            Step::raw(QomGet, line),
        ])
        .await;

        let actuator = Arc::new(actuator);

        let waiter = tokio::spawn({
            let actuator = Arc::clone(&actuator);

            async move { actuator.changed().await }
        });

        settled(&server, STARTUP.len() + 1).await;

        let error = actuator
            .reading()
            .await
            .expect_err("the reply is over the cap");

        assert!(
            error.to_string().contains("grew past the"),
            "unexpected error: {error}"
        );

        let waited = waiter
            .await
            .expect("the waiter ran to the end")
            .expect_err("the connection broke under the waiter");

        assert!(
            waited.to_string().contains("grew past the"),
            "unexpected error: {waited}"
        );
    }

    #[tokio::test]
    async fn a_shared_socket_is_greeted_again_for_every_command() {
        let server = FakeQmp::start([
            shared(DEVICE, BOOT, 0, 0),
            vec![Step::returns(QomGet, json!(256 * MIB))],
            vec![Step::returns(QomSet, json!({}))],
        ]);

        let actuator = open(&server, polling()).await;

        assert_eq!(actuator.reading().await.unwrap().size, BOOT + 256 * MIB);
        actuator.set_requested(BOOT + 512 * MIB).await.unwrap();

        assert_eq!(server.connections(), 3);
        assert_eq!(
            server.commands(),
            [
                Capabilities,
                QueryMemoryDevices,
                QueryMemorySizeSummary,
                QomGet,
                QomGet,
                Capabilities,
                QomGet,
                Capabilities,
                QomSet,
            ]
        );
    }

    #[tokio::test]
    async fn a_shared_socket_resamples_the_size_until_a_resize_lands() {
        let server = FakeQmp::start([
            shared(DEVICE, BOOT, 256 * MIB, 256 * MIB),
            vec![Step::returns(QomSet, json!({}))],
            vec![Step::returns(QomGet, json!(256 * MIB))],
            vec![Step::returns(QomGet, json!(256 * MIB))],
            vec![Step::returns(QomGet, json!(768 * MIB))],
        ]);

        let actuator = open(&server, polling()).await;

        actuator.set_requested(BOOT + 768 * MIB).await.unwrap();

        assert_eq!(actuator.changed().await.unwrap().size, BOOT + 768 * MIB);
        assert_eq!(server.connections(), 5);
    }

    #[tokio::test]
    async fn a_shared_socket_reports_every_size_a_resize_passes_through() {
        let server = FakeQmp::start([
            shared(DEVICE, BOOT, 0, 0),
            vec![Step::returns(QomSet, json!({}))],
            vec![Step::returns(QomGet, json!(256 * MIB))],
            vec![Step::returns(QomGet, json!(512 * MIB))],
        ]);

        let actuator = open(&server, polling()).await;

        actuator.set_requested(BOOT + 512 * MIB).await.unwrap();

        assert_eq!(actuator.changed().await.unwrap().size, BOOT + 256 * MIB);
        assert_eq!(actuator.changed().await.unwrap().size, BOOT + 512 * MIB);
        assert_eq!(server.connections(), 4);
    }

    #[tokio::test]
    async fn a_shared_socket_is_left_alone_while_no_resize_is_in_flight() {
        let server = FakeQmp::start([
            shared(DEVICE, BOOT, 0, 0),
            vec![Step::returns(QomSet, json!({}))],
            vec![Step::returns(QomGet, json!(512 * MIB))],
        ]);

        let actuator = open(&server, polling()).await;

        assert!(
            timeout(QUIET, actuator.changed()).await.is_err(),
            "the size was polled with no resize asked for"
        );
        assert_eq!(server.connections(), 1);

        actuator.set_requested(BOOT + 512 * MIB).await.unwrap();

        assert_eq!(actuator.changed().await.unwrap().size, BOOT + 512 * MIB);

        assert!(
            timeout(QUIET, actuator.changed()).await.is_err(),
            "the size was polled after the resize landed"
        );
        assert_eq!(server.connections(), 3);
    }

    #[tokio::test]
    async fn a_shared_socket_takes_up_a_resize_asked_for_before_it_connected() {
        let server = FakeQmp::start([
            shared(DEVICE, BOOT, 0, 512 * MIB),
            vec![Step::returns(QomGet, json!(256 * MIB))],
            vec![Step::returns(QomGet, json!(512 * MIB))],
        ]);

        let actuator = open(&server, polling()).await;

        assert_eq!(actuator.changed().await.unwrap().size, BOOT + 256 * MIB);
        assert_eq!(actuator.changed().await.unwrap().size, BOOT + 512 * MIB);
    }

    #[tokio::test]
    async fn a_reply_over_the_line_cap_is_refused() {
        let line = format!(r#"{{"return":"{}","id":1}}"#, "x".repeat(MAX_LINE_BYTES));

        let server = FakeQmp::start([shared(DEVICE, BOOT, 0, 0), vec![Step::raw(QomGet, line)]]);
        let actuator = open(&server, polling()).await;

        let error = actuator
            .reading()
            .await
            .expect_err("the reply is over the cap");

        assert!(
            error
                .to_string()
                .contains(&format!("grew past the {MAX_LINE_BYTES} byte cap")),
            "unexpected error: {error}"
        );
    }
}
