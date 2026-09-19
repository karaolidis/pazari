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
    sync::Mutex,
    time::{sleep, timeout},
};
use tracing::debug;

use crate::runtime::{
    actuator::Actuator,
    error::RuntimeError,
    qmp::{
        Endpoint, Held, MemoryDevice, MemorySummary, QOM_GET, QOM_SET, QUERY_DEVICES,
        QUERY_SUMMARY, QmpError, Session, qom_path,
    },
};

const HANDSHAKE: &str = "the handshake";

const SIZE: &str = "size";
const REQUESTED_SIZE: &str = "requested-size";
const VIRTIO_MEM: &str = "virtio-mem";

/// How the adapter uses the QMP socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Held,

    PerOperation {
        /// How often [`Actuator::changed`] resamples the size.
        poll: Interval,
    },
}

/// One guest's virtio-mem device over QMP.
#[derive(Debug)]
pub struct QmpActuator {
    path: PathBuf,
    device: String,
    qom: String,
    mode: Mode,
    deadline: Interval,
    boot: AtomicU64,
    last: AtomicU64,
    held: Mutex<Option<Arc<Held>>>,
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

        let mut session = deadlined(Session::open(&path), deadline, HANDSHAKE, &path).await?;

        let boot = verify(&mut session, &device, deadline).await?;
        let size = boot.saturating_add(read(&mut session, &qom, SIZE, deadline).await?);

        let held = match mode {
            Mode::Held => Some(Arc::new(Held::take(session, &device))),
            Mode::PerOperation { .. } => None,
        };

        Ok(Self {
            path,
            device,
            qom,
            mode,
            deadline,
            boot: AtomicU64::new(boot),
            last: AtomicU64::new(size),
            held: Mutex::new(held),
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
    pub fn boot(&self) -> u64 {
        self.boot.load(Ordering::Relaxed)
    }

    async fn execute(&self, command: &'static str, arguments: Value) -> Result<Value, QmpError> {
        let mut wire = self.wire().await?;

        deadlined(
            wire.execute(command, arguments),
            self.deadline,
            command,
            &self.path,
        )
        .await
    }

    async fn wire(&self) -> Result<Wire, QmpError> {
        match self.mode {
            Mode::Held => Ok(Wire::Held(self.client().await?)),
            Mode::PerOperation { .. } => Ok(Wire::Fresh(self.open().await?)),
        }
    }

    async fn open(&self) -> Result<Session, QmpError> {
        deadlined(
            Session::open(&self.path),
            self.deadline,
            HANDSHAKE,
            &self.path,
        )
        .await
    }

    async fn client(&self) -> Result<Arc<Held>, QmpError> {
        if let Some(live) = self.live().await {
            return Ok(live);
        }

        debug!(path = %self.path.display(), "reopening the QMP connection");

        let mut session = self.open().await?;
        let boot = verify(&mut session, &self.device, self.deadline).await?;
        let live = Arc::new(Held::take(session, &self.device));

        let mut held = self.held.lock().await;

        if let Some(other) = held.as_ref().filter(|other| !other.is_closed()) {
            let other = Arc::clone(other);
            drop(held);

            return Ok(other);
        }

        self.boot.store(boot, Ordering::Relaxed);
        *held = Some(Arc::clone(&live));
        drop(held);

        Ok(live)
    }

    async fn live(&self) -> Option<Arc<Held>> {
        let held = self.held.lock().await;

        let live = held
            .as_ref()
            .filter(|live| !live.is_closed())
            .map(Arc::clone);

        drop(held);

        live
    }

    async fn property(&self, property: &'static str) -> Result<u64, QmpError> {
        read(&mut self.wire().await?, &self.qom, property, self.deadline).await
    }

    fn report(&self, size: u64) -> Option<u64> {
        (self.last.swap(size, Ordering::Relaxed) != size).then_some(size)
    }

    async fn resampled(&self, poll: Interval) -> Result<u64, RuntimeError> {
        loop {
            if let Some(size) = self.report(self.size().await?) {
                return Ok(size);
            }

            sleep(poll.get()).await;
        }
    }

    async fn awaited(&self) -> Result<u64, RuntimeError> {
        loop {
            let mut client = self.client().await?;
            let mut sizes = client.watch();

            let sampled = self
                .boot()
                .saturating_add(read(&mut client, &self.qom, SIZE, self.deadline).await?);

            if let Some(size) = self.report(sampled) {
                return Ok(size);
            }

            let raised = self
                .boot()
                .saturating_add(client.changed(&mut sizes).await?);

            if let Some(size) = self.report(raised) {
                return Ok(size);
            }
        }
    }
}

impl Actuator for QmpActuator {
    async fn size(&self) -> Result<u64, RuntimeError> {
        let size = self.property(SIZE).await?;

        Ok(self.boot().saturating_add(size))
    }

    async fn requested(&self) -> Result<u64, RuntimeError> {
        let requested = self.property(REQUESTED_SIZE).await?;

        Ok(self.boot().saturating_add(requested))
    }

    async fn set_requested(&self, bytes: u64) -> Result<(), RuntimeError> {
        let boot = self.boot();

        if bytes < boot {
            return Err(QmpError::BelowBoot { bytes, boot }.into());
        }

        let arguments = json!({
            "path": self.qom,
            "property": REQUESTED_SIZE,
            "value": bytes.saturating_sub(boot),
        });

        match self.execute(QOM_SET, arguments).await {
            Ok(_) => Ok(()),
            Err(QmpError::Refused { command, reason }) if command == QOM_SET => {
                Err(RuntimeError::Device { bytes, reason })
            }
            Err(error) => Err(error.into()),
        }
    }

    async fn changed(&self) -> Result<u64, RuntimeError> {
        match self.mode {
            Mode::Held => self.awaited().await,
            Mode::PerOperation { poll } => self.resampled(poll).await,
        }
    }
}

async fn deadlined<T>(
    operation: impl Future<Output = Result<T, QmpError>>,
    deadline: Interval,
    what: &'static str,
    path: &Path,
) -> Result<T, QmpError> {
    timeout(deadline.get(), operation)
        .await
        .map_err(|_| QmpError::Timeout {
            what,
            path: path.to_path_buf(),
            deadline,
        })?
}

async fn verify(session: &mut Session, device: &str, deadline: Interval) -> Result<u64, QmpError> {
    let path = session.path().to_path_buf();

    let value = deadlined(
        session.execute(QUERY_DEVICES, Value::Null),
        deadline,
        QUERY_DEVICES,
        &path,
    )
    .await?;

    let devices: Vec<MemoryDevice> =
        serde_json::from_value(value).map_err(|source| QmpError::Protocol {
            path: path.clone(),
            reason: format!("{QUERY_DEVICES} answered with {source}"),
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

    let value = deadlined(
        session.execute(QUERY_SUMMARY, Value::Null),
        deadline,
        QUERY_SUMMARY,
        &path,
    )
    .await?;

    let summary: MemorySummary =
        serde_json::from_value(value).map_err(|source| QmpError::Protocol {
            path,
            reason: format!("{QUERY_SUMMARY} answered with {source}"),
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

    async fn execute(
        &mut self,
        command: &'static str,
        arguments: Value,
    ) -> Result<Value, QmpError> {
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
        endpoint.execute(QOM_GET, json!({ "path": qom, "property": property })),
        deadline,
        QOM_GET,
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
            qmp::{CAPABILITIES, MAX_LINE_BYTES},
            testing::qmp::{
                FakeQmp, Step, devices, resized, resized_anonymously, startup, verified,
            },
        },
    };

    const DEVICE: &str = "vm0";
    const OTHER: &str = "vm1";

    const QUIET: Duration = Duration::from_millis(50);

    const STARTUP: [&str; 4] = [CAPABILITIES, QUERY_DEVICES, QUERY_SUMMARY, QOM_GET];

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
            Step::returns(QOM_GET, json!(1024 * MIB)),
            Step::returns(QOM_GET, json!(1536 * MIB)),
        ])
        .await;

        assert_eq!(actuator.size().await.unwrap(), BOOT + 1024 * MIB);
        assert_eq!(actuator.requested().await.unwrap(), BOOT + 1536 * MIB);

        assert_eq!(
            server.arguments(QOM_GET),
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
        let (server, actuator) = held(vec![Step::returns(QOM_SET, json!({}))]).await;

        actuator.set_requested(BOOT + 512 * MIB).await.unwrap();

        assert_eq!(
            server.arguments(QOM_SET),
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
            QOM_SET,
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
            startup(DEVICE, BOOT, 0),
            vec![Step::refuses(CAPABILITIES, "this socket is read-only")],
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
            QUERY_DEVICES,
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
            QUERY_DEVICES,
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
        let server = FakeQmp::start([startup(DEVICE, BOOT, 0), vec![Step::Ignore]]);

        let actuator = QmpActuator::connect(server.path(), DEVICE, polling(), impatient())
            .await
            .expect("the device is there");

        let error = actuator.size().await.expect_err("the server never answers");

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
        script.extend([Step::Ignore, Step::returns(QOM_GET, json!(256 * MIB))]);

        let server = FakeQmp::start([script]);

        let actuator = QmpActuator::connect(server.path(), DEVICE, Mode::Held, impatient())
            .await
            .expect("the device is there");

        actuator.size().await.expect_err("the server never answers");

        assert_eq!(actuator.size().await.unwrap(), BOOT + 256 * MIB);
        assert_eq!(server.connections(), 1);
        assert_eq!(server.commands().len(), STARTUP.len() + 2);
    }

    #[tokio::test]
    async fn a_size_change_event_wakes_the_waiter() {
        let (_server, actuator) = held(vec![
            Step::returns(QOM_SET, json!({})),
            Step::Emit(resized(DEVICE, 1024 * MIB)),
            Step::returns(QOM_GET, json!(1024 * MIB)),
        ])
        .await;

        actuator.set_requested(BOOT + 1024 * MIB).await.unwrap();

        assert_eq!(actuator.changed().await.unwrap(), BOOT + 1024 * MIB);
    }

    #[tokio::test]
    async fn an_event_that_only_carries_a_qom_path_still_wakes_the_waiter() {
        let (_server, actuator) = held(vec![
            Step::Emit(resized_anonymously(&qom_path(DEVICE), 2048 * MIB)),
            Step::returns(QOM_GET, json!(2048 * MIB)),
        ])
        .await;

        assert_eq!(actuator.changed().await.unwrap(), BOOT + 2048 * MIB);
    }

    #[tokio::test]
    async fn an_event_between_two_calls_is_reported_by_the_second() {
        let (_server, actuator) = held(vec![
            Step::returns(QOM_GET, json!(256 * MIB)),
            Step::Emit(resized(DEVICE, 512 * MIB)),
            Step::returns(QOM_GET, json!(512 * MIB)),
        ])
        .await;

        assert_eq!(actuator.changed().await.unwrap(), BOOT + 256 * MIB);
        assert_eq!(actuator.changed().await.unwrap(), BOOT + 512 * MIB);
    }

    #[tokio::test]
    async fn nothing_but_this_device_wakes_the_waiter() {
        let (_server, actuator) = held(vec![
            Step::Emit(resized(OTHER, 4096 * MIB)),
            Step::Emit(resized_anonymously(&qom_path(OTHER), 3072 * MIB)),
            Step::Emit(stopped()),
            Step::returns(QOM_GET, json!(0)),
            Step::returns(QOM_GET, json!(0)),
        ])
        .await;

        assert_eq!(actuator.size().await.unwrap(), BOOT);

        assert!(
            timeout(QUIET, actuator.changed()).await.is_err(),
            "another device's event woke the waiter"
        );
    }

    #[tokio::test]
    async fn a_lost_connection_wakes_the_waiter_and_the_next_command_reconnects() {
        let mut first = startup(DEVICE, BOOT, 0);
        first.push(Step::Hangup);

        let mut second = verified(DEVICE, BOOT);
        second.push(Step::returns(QOM_GET, json!(256 * MIB)));

        let server = FakeQmp::start([first, second]);
        let actuator = open(&server, Mode::Held).await;

        let error = actuator.changed().await.expect_err("the server hung up");

        assert_eq!(
            error.to_string(),
            format!("the QMP connection to {} closed", server.path().display())
        );

        assert_eq!(actuator.size().await.unwrap(), BOOT + 256 * MIB);
        assert_eq!(server.connections(), 2);
        assert_eq!(
            server.commands(),
            [
                CAPABILITIES,
                QUERY_DEVICES,
                QUERY_SUMMARY,
                QOM_GET,
                CAPABILITIES,
                QUERY_DEVICES,
                QUERY_SUMMARY,
                QOM_GET,
            ]
        );
    }

    #[tokio::test]
    async fn a_reconnect_stands_the_sizes_on_the_boot_memory_it_reads_again() {
        let mut first = startup(DEVICE, BOOT, 0);
        first.push(Step::Hangup);

        let mut second = verified(DEVICE, BOOT + 512 * MIB);
        second.push(Step::returns(QOM_GET, json!(256 * MIB)));

        let server = FakeQmp::start([first, second]);
        let actuator = open(&server, Mode::Held).await;

        actuator.size().await.expect_err("the server hung up");

        assert_eq!(actuator.size().await.unwrap(), BOOT + 512 * MIB + 256 * MIB);
        assert_eq!(actuator.boot(), BOOT + 512 * MIB);
    }

    #[tokio::test]
    async fn a_size_that_moved_during_an_outage_is_reported_after_the_reconnect() {
        let mut first = startup(DEVICE, BOOT, 0);
        first.push(Step::Hangup);

        let mut second = verified(DEVICE, BOOT);
        second.push(Step::returns(QOM_GET, json!(1024 * MIB)));

        let server = FakeQmp::start([first, second]);
        let actuator = open(&server, Mode::Held).await;

        actuator.changed().await.expect_err("the server hung up");

        assert_eq!(actuator.changed().await.unwrap(), BOOT + 1024 * MIB);
        assert_eq!(server.connections(), 2);
    }

    #[tokio::test]
    async fn a_broken_frame_reaches_everyone_waiting() {
        let line = format!(r#"{{"return":"{}","id":1}}"#, "x".repeat(MAX_LINE_BYTES));

        let (server, actuator) = held(vec![
            Step::returns(QOM_GET, json!(0)),
            Step::raw(QOM_GET, line),
        ])
        .await;

        let actuator = Arc::new(actuator);

        let waiter = tokio::spawn({
            let actuator = Arc::clone(&actuator);

            async move { actuator.changed().await }
        });

        settled(&server, STARTUP.len() + 1).await;

        let error = actuator
            .size()
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
            startup(DEVICE, BOOT, 0),
            vec![Step::returns(QOM_GET, json!(256 * MIB))],
            vec![Step::returns(QOM_SET, json!({}))],
        ]);

        let actuator = open(&server, polling()).await;

        assert_eq!(actuator.size().await.unwrap(), BOOT + 256 * MIB);
        actuator.set_requested(BOOT + 512 * MIB).await.unwrap();

        assert_eq!(server.connections(), 3);
        assert_eq!(
            server.commands(),
            [
                CAPABILITIES,
                QUERY_DEVICES,
                QUERY_SUMMARY,
                QOM_GET,
                CAPABILITIES,
                QOM_GET,
                CAPABILITIES,
                QOM_SET,
            ]
        );
    }

    #[tokio::test]
    async fn a_shared_socket_resamples_the_size_until_it_moves() {
        let server = FakeQmp::start([
            startup(DEVICE, BOOT, 256 * MIB),
            vec![Step::returns(QOM_GET, json!(256 * MIB))],
            vec![Step::returns(QOM_GET, json!(256 * MIB))],
            vec![Step::returns(QOM_GET, json!(768 * MIB))],
        ]);

        let actuator = open(&server, polling()).await;

        assert_eq!(actuator.changed().await.unwrap(), BOOT + 768 * MIB);
        assert_eq!(server.connections(), 4);
    }

    #[tokio::test]
    async fn a_size_that_moved_between_calls_is_reported_at_once() {
        let server = FakeQmp::start([
            startup(DEVICE, BOOT, 0),
            vec![Step::returns(QOM_GET, json!(256 * MIB))],
            vec![Step::returns(QOM_GET, json!(512 * MIB))],
        ]);

        let actuator = open(&server, polling()).await;

        assert_eq!(actuator.changed().await.unwrap(), BOOT + 256 * MIB);
        assert_eq!(actuator.changed().await.unwrap(), BOOT + 512 * MIB);
        assert_eq!(server.connections(), 3);
    }

    #[tokio::test]
    async fn a_reply_over_the_line_cap_is_refused() {
        let line = format!(r#"{{"return":"{}","id":1}}"#, "x".repeat(MAX_LINE_BYTES));

        let server = FakeQmp::start([startup(DEVICE, BOOT, 0), vec![Step::raw(QOM_GET, line)]]);
        let actuator = open(&server, polling()).await;

        let error = actuator
            .size()
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
