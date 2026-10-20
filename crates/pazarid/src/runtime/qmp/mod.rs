pub mod error;
pub mod wire;

use std::{
    collections::HashMap,
    future::Future,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
};

use serde::Deserialize;
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader},
    net::{
        UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};
use tracing::{debug, warn};

use crate::runtime::qmp::wire::MEMORY_DEVICE_SIZE_CHANGE;
pub use crate::runtime::qmp::{
    error::{Operation, QmpError, SocketOp},
    wire::{
        Command, DeviceData, Event, Failure, Fault, Greeting, Handshake, Incoming, MemoryDevice,
        MemorySummary, Outcome, QmpCommand, Reply, SizeChange,
    },
};

pub const MAX_LINE_BYTES: usize = 1 << 20;

const OUTBOX: usize = 32;

/// Where QEMU hangs a device given an id on the command line.
#[must_use]
pub fn qom_path(device: &str) -> String {
    format!("/machine/peripheral/{device}")
}

/// Newline-delimited frames off a QMP socket.
#[derive(Debug)]
pub struct Lines<R> {
    reader: BufReader<R>,
    path: PathBuf,
    buf: Vec<u8>,

    /// Why an earlier frame broke the protocol.
    broken: Option<String>,
}

impl<R: AsyncRead + Unpin> Lines<R> {
    #[must_use]
    pub fn new(reader: R, path: impl Into<PathBuf>) -> Self {
        Self {
            reader: BufReader::new(reader),
            path: path.into(),
            buf: Vec::new(),
            broken: None,
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn recv(&mut self) -> Result<Option<Incoming>, QmpError> {
        if let Some(reason) = &self.broken {
            return Err(QmpError::Protocol {
                path: self.path.clone(),
                reason: reason.clone(),
            });
        }

        if !self.fill().await? {
            return Ok(None);
        }

        match serde_json::from_slice(&self.buf) {
            Ok(frame) => Ok(Some(frame)),
            Err(source) => Err(self.broke(&format!("a frame did not parse: {source}"))),
        }
    }

    async fn fill(&mut self) -> Result<bool, QmpError> {
        self.buf.clear();

        loop {
            let available = self
                .reader
                .fill_buf()
                .await
                .map_err(|source| QmpError::Socket {
                    op: SocketOp::Read,
                    path: self.path.clone(),
                    source,
                })?;

            if available.is_empty() {
                if self.buf.is_empty() {
                    return Ok(false);
                }

                let len = self.buf.len();

                return Err(self.broke(&format!("the connection ended {len} bytes into a frame")));
            }

            let end = available.iter().position(|byte| *byte == b'\n');
            let take = end.unwrap_or(available.len());

            if self.buf.len().saturating_add(take) > MAX_LINE_BYTES {
                return Err(self.broke(&format!("a frame grew past the {MAX_LINE_BYTES} byte cap")));
            }

            self.buf.extend_from_slice(&available[..take]);
            self.reader.consume(if end.is_some() {
                take.saturating_add(1)
            } else {
                take
            });

            if end.is_some() {
                return Ok(true);
            }
        }
    }

    fn broke(&mut self, reason: &str) -> QmpError {
        self.broken = Some(reason.to_owned());

        QmpError::Protocol {
            path: self.path.clone(),
            reason: reason.to_owned(),
        }
    }
}

/// A connection commands can go down.
pub trait Endpoint: Send {
    fn path(&self) -> &Path;

    fn execute(
        &mut self,
        command: QmpCommand,
        arguments: Value,
    ) -> impl Future<Output = Result<Value, QmpError>> + Send;
}

#[derive(Debug)]
pub struct Session {
    lines: Lines<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    next: u64,
}

impl Session {
    pub async fn open(path: &Path) -> Result<Self, QmpError> {
        let stream = UnixStream::connect(path)
            .await
            .map_err(|source| QmpError::Socket {
                op: SocketOp::Connect,
                path: path.to_path_buf(),
                source,
            })?;

        let (reader, writer) = stream.into_split();

        let mut session = Self {
            lines: Lines::new(reader, path),
            writer,
            next: 0,
        };

        if !matches!(session.frame().await?, Incoming::Greeting(_)) {
            return Err(session.broke("the server spoke before it greeted"));
        }

        session
            .execute(QmpCommand::Capabilities, Value::Null)
            .await?;

        Ok(session)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        self.lines.path()
    }

    pub async fn execute(
        &mut self,
        command: QmpCommand,
        arguments: Value,
    ) -> Result<Value, QmpError> {
        let id = self.next;
        self.next = self.next.saturating_add(1);

        self.send(&encode(command, id, arguments)).await?;

        loop {
            match self.frame().await? {
                Incoming::Reply(reply) if reply.id == id => {
                    return reply.outcome.into_result(command);
                }
                Incoming::Event(_) => {}
                Incoming::Fault(fault) => {
                    let desc = fault.error.desc;
                    return Err(self.broke(&format!("the server rejected a frame: {desc}")));
                }
                Incoming::Reply(reply) => {
                    let stray = reply.id;
                    return Err(self.broke(&format!(
                        "a reply carried id {stray} while {id} was outstanding"
                    )));
                }
                Incoming::Greeting(_) => return Err(self.broke("the server greeted twice")),
            }
        }
    }

    fn into_parts(self) -> (Lines<OwnedReadHalf>, OwnedWriteHalf, u64) {
        (self.lines, self.writer, self.next)
    }

    async fn frame(&mut self) -> Result<Incoming, QmpError> {
        self.lines.recv().await?.ok_or_else(|| QmpError::Closed {
            path: self.path().to_path_buf(),
        })
    }

    async fn send(&mut self, line: &[u8]) -> Result<(), QmpError> {
        let path = self.lines.path();

        self.writer
            .write_all(line)
            .await
            .map_err(|source| QmpError::Socket {
                op: SocketOp::Write,
                path: path.to_path_buf(),
                source,
            })?;

        self.writer
            .flush()
            .await
            .map_err(|source| QmpError::Socket {
                op: SocketOp::Write,
                path: path.to_path_buf(),
                source,
            })
    }

    fn broke(&self, reason: &str) -> QmpError {
        QmpError::Protocol {
            path: self.path().to_path_buf(),
            reason: reason.to_owned(),
        }
    }
}

impl Endpoint for Session {
    fn path(&self) -> &Path {
        Self::path(self)
    }

    fn execute(
        &mut self,
        command: QmpCommand,
        arguments: Value,
    ) -> impl Future<Output = Result<Value, QmpError>> + Send {
        Self::execute(self, command, arguments)
    }
}

#[derive(Debug)]
pub struct Held {
    path: PathBuf,
    writes: mpsc::Sender<Vec<u8>>,
    sizes: watch::Receiver<u64>,
    pending: Arc<Pending>,
    next: AtomicU64,
    reader: JoinHandle<()>,
    writer: JoinHandle<()>,
}

impl Held {
    #[must_use]
    pub fn take(session: Session, device: &str) -> Self {
        let (lines, half, next) = session.into_parts();
        let path = lines.path().to_path_buf();

        let pending = Arc::new(Pending::new());
        let (sizes, receiver) = watch::channel(0);
        let (writes, outbox) = mpsc::channel(OUTBOX);

        let reader = tokio::spawn(route(
            lines,
            Arc::clone(&pending),
            sizes,
            Device::new(device),
        ));

        let writer = tokio::spawn(feed(half, outbox, Arc::clone(&pending), path.clone()));

        Self {
            path,
            writes,
            sizes: receiver,
            pending,
            next: AtomicU64::new(next),
            reader,
            writer,
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.pending.is_closed()
    }

    #[must_use]
    pub fn watch(&self) -> watch::Receiver<u64> {
        let mut sizes = self.sizes.clone();
        sizes.mark_unchanged();

        sizes
    }

    pub async fn execute(&self, command: QmpCommand, arguments: Value) -> Result<Value, QmpError> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);

        let mut registered = Registered::new(&self.pending, id).ok_or_else(|| self.fatal())?;

        if self
            .writes
            .send(encode(command, id, arguments))
            .await
            .is_err()
        {
            return Err(self.fatal());
        }

        registered
            .waited()
            .await
            .ok_or_else(|| self.fatal())?
            .into_result(command)
    }

    pub async fn changed(&self, sizes: &mut watch::Receiver<u64>) -> Result<u64, QmpError> {
        sizes.changed().await.map_err(|_| self.fatal())?;

        let size = *sizes.borrow_and_update();

        Ok(size)
    }

    fn fatal(&self) -> QmpError {
        self.pending.fate().map_or_else(
            || QmpError::Closed {
                path: self.path.clone(),
            },
            QmpError::Fatal,
        )
    }
}

impl Endpoint for Arc<Held> {
    fn path(&self) -> &Path {
        Held::path(self)
    }

    fn execute(
        &mut self,
        command: QmpCommand,
        arguments: Value,
    ) -> impl Future<Output = Result<Value, QmpError>> + Send {
        Held::execute(self, command, arguments)
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.reader.abort();
        self.writer.abort();
    }
}

#[derive(Debug)]
struct Registered<'a> {
    pending: &'a Pending,
    id: u64,
    waiter: oneshot::Receiver<Outcome>,
}

impl<'a> Registered<'a> {
    fn new(pending: &'a Pending, id: u64) -> Option<Self> {
        pending.register(id).map(|waiter| Self {
            pending,
            id,
            waiter,
        })
    }

    async fn waited(&mut self) -> Option<Outcome> {
        (&mut self.waiter).await.ok()
    }
}

impl Drop for Registered<'_> {
    fn drop(&mut self) {
        self.pending.forget(self.id);
    }
}

#[derive(Debug)]
struct Pending {
    state: Mutex<State>,
}

#[derive(Debug)]
enum State {
    Open(HashMap<u64, oneshot::Sender<Outcome>>),
    Closed(Option<Arc<QmpError>>),
}

impl Pending {
    fn new() -> Self {
        Self {
            state: Mutex::new(State::Open(HashMap::new())),
        }
    }

    fn register(&self, id: u64) -> Option<oneshot::Receiver<Outcome>> {
        let (sender, receiver) = oneshot::channel();

        let mut state = self.locked();
        let State::Open(waiters) = &mut *state else {
            return None;
        };

        waiters.insert(id, sender);
        drop(state);

        Some(receiver)
    }

    fn forget(&self, id: u64) {
        let mut state = self.locked();

        if let State::Open(waiters) = &mut *state {
            waiters.remove(&id);
        }

        drop(state);
    }

    /// A reply nothing waits for answers a command given up on.
    fn resolve(&self, id: u64, outcome: Outcome) {
        let mut state = self.locked();

        let waiter = match &mut *state {
            State::Open(waiters) => waiters.remove(&id),
            State::Closed(_) => None,
        };

        drop(state);

        if let Some(waiter) = waiter {
            let _ = waiter.send(outcome);
        } else {
            debug!(id, "QMP reply to a command given up on");
        }
    }

    fn close(&self, fate: Option<Arc<QmpError>>) {
        let mut state = self.locked();

        if matches!(*state, State::Open(_)) {
            *state = State::Closed(fate);
        }

        drop(state);
    }

    fn is_closed(&self) -> bool {
        let state = self.locked();
        let closed = matches!(*state, State::Closed(_));
        drop(state);

        closed
    }

    fn fate(&self) -> Option<Arc<QmpError>> {
        let state = self.locked();

        let fate = match &*state {
            State::Open(_) => None,
            State::Closed(fate) => fate.clone(),
        };

        drop(state);

        fate
    }

    fn locked(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[derive(Debug)]
struct Device {
    id: String,
    qom: String,
}

impl Device {
    fn new(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            qom: qom_path(id),
        }
    }

    fn resized(&self, event: &Event) -> Option<u64> {
        if event.event != MEMORY_DEVICE_SIZE_CHANGE {
            return None;
        }

        let change = SizeChange::deserialize(&event.data).ok()?;

        let ours = match &change.id {
            Some(id) => *id == self.id,
            None => change.qom_path.as_deref() == Some(self.qom.as_str()),
        };

        ours.then_some(change.size)
    }
}

async fn route(
    mut lines: Lines<OwnedReadHalf>,
    pending: Arc<Pending>,
    sizes: watch::Sender<u64>,
    device: Device,
) {
    let fate = loop {
        match lines.recv().await {
            Ok(Some(Incoming::Reply(reply))) => pending.resolve(reply.id, reply.outcome),
            Ok(Some(Incoming::Event(event))) => {
                if let Some(size) = device.resized(&event) {
                    debug!(size, device = device.id, "device size changed");

                    let _ = sizes.send(size);
                }
            }
            Ok(Some(Incoming::Fault(fault))) => {
                warn!(desc = %fault.error.desc, "QMP server rejected a frame");
            }
            Ok(Some(Incoming::Greeting(_))) => warn!("duplicate QMP greeting"),
            Ok(None) => {
                debug!("QMP server closed the connection");
                break None;
            }
            Err(error) => {
                warn!(%error, "QMP connection failed");
                break Some(Arc::new(error));
            }
        }
    };

    pending.close(fate);
}

async fn feed(
    mut writer: OwnedWriteHalf,
    mut outbox: mpsc::Receiver<Vec<u8>>,
    pending: Arc<Pending>,
    path: PathBuf,
) {
    while let Some(line) = outbox.recv().await {
        let written = match writer.write_all(&line).await {
            Ok(()) => writer.flush().await,
            Err(source) => Err(source),
        };

        let Err(source) = written else {
            continue;
        };

        warn!(%source, "QMP connection refused a command");

        pending.close(Some(Arc::new(QmpError::Socket {
            op: SocketOp::Write,
            path,
            source,
        })));

        return;
    }
}

fn encode(command: QmpCommand, id: u64, arguments: Value) -> Vec<u8> {
    let Ok(mut line) = serde_json::to_vec(&Command {
        execute: command,
        id,
        arguments,
    }) else {
        unreachable!("a command holds nothing serde_json cannot write");
    };

    line.push(b'\n');

    line
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncWriteExt, duplex};

    use super::*;

    const PATH: &str = "/run/pazari/vm0.qmp";

    fn lines(text: &str) -> Lines<impl AsyncRead + Unpin + use<>> {
        let (mut server, client) = duplex(64 * 1024);
        let text = text.to_owned();

        tokio::spawn(async move {
            let _ = server.write_all(text.as_bytes()).await;
            let _ = server.flush().await;
        });

        Lines::new(client, PATH)
    }

    #[test]
    fn a_device_hangs_where_qemu_puts_it() {
        assert_eq!(qom_path("vm0"), "/machine/peripheral/vm0");
    }

    #[tokio::test]
    async fn frames_split_across_reads_arrive_whole() {
        let (mut server, client) = duplex(8);

        tokio::spawn(async move {
            for chunk in [r#"{"return":{},"i"#, r#"d":4}"#, "\n"] {
                let _ = server.write_all(chunk.as_bytes()).await;
                let _ = server.flush().await;
            }
        });

        let mut lines = Lines::new(client, PATH);

        assert!(matches!(
            lines.recv().await.unwrap(),
            Some(Incoming::Reply(Reply { id: 4, .. }))
        ));
        assert!(lines.recv().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn an_over_long_frame_is_refused() {
        let mut lines = lines(&format!("{}\n", "x".repeat(MAX_LINE_BYTES + 1)));

        let error = lines.recv().await.expect_err("the frame is over the cap");

        assert_eq!(
            error.to_string(),
            format!(
                "the QMP server at {PATH} broke the protocol: \
                 a frame grew past the {MAX_LINE_BYTES} byte cap"
            )
        );
    }

    #[tokio::test]
    async fn a_reader_that_broke_never_reads_again() {
        let mut lines = lines(&format!(
            "{}\n{}\n",
            "x".repeat(MAX_LINE_BYTES + 1),
            r#"{"return":{},"id":1}"#
        ));

        lines.recv().await.expect_err("the frame is over the cap");

        let error = lines.recv().await.expect_err("the reader is broken");

        assert_eq!(
            error.to_string(),
            format!(
                "the QMP server at {PATH} broke the protocol: \
                 a frame grew past the {MAX_LINE_BYTES} byte cap"
            ),
            "a later read lost what broke the reader",
        );
    }

    #[tokio::test]
    async fn a_frame_of_exactly_the_cap_is_read() {
        let padding = "x".repeat(MAX_LINE_BYTES - r#"{"return":"","id":1}"#.len());
        let frame = format!(r#"{{"return":"{padding}","id":1}}"#);

        assert_eq!(frame.len(), MAX_LINE_BYTES);

        let mut lines = lines(&format!("{frame}\n"));

        assert!(matches!(
            lines.recv().await.unwrap(),
            Some(Incoming::Reply(Reply { id: 1, .. }))
        ));
    }

    #[tokio::test]
    async fn a_frame_that_is_not_json_names_itself() {
        let mut lines = lines("not json at all\n");

        let error = lines.recv().await.expect_err("the frame is not a frame");

        assert!(
            error.to_string().contains("a frame did not parse"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn a_connection_that_ends_mid_frame_says_so() {
        let mut lines = lines(r#"{"return":{},"id":1}"#);

        let error = lines.recv().await.expect_err("the frame never ended");

        assert!(
            error
                .to_string()
                .ends_with("the connection ended 20 bytes into a frame"),
            "unexpected error: {error}"
        );
    }
}
