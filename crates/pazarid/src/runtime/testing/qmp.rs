use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::Deserialize;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    net::{
        UnixListener, UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
    task::JoinHandle,
};

use crate::runtime::qmp::{QmpCommand, qom_path};

const COMMANDS: [QmpCommand; 5] = [
    QmpCommand::Capabilities,
    QmpCommand::QueryMemoryDevices,
    QmpCommand::QueryMemorySizeSummary,
    QmpCommand::QomGet,
    QmpCommand::QomSet,
];

pub const GREETING: &str = r#"{"QMP":{"version":{"qemu":{"major":10,"minor":1,"micro":0},"package":"pazari"},"capabilities":["oob"]}}"#;

#[derive(Debug, Clone)]
pub enum Answer {
    Return(Value),

    Refuse(String),

    Raw(String),
}

#[derive(Debug, Clone)]
pub enum Step {
    Reply { command: QmpCommand, answer: Answer },

    Emit(String),

    Ignore,

    Hangup,
}

impl Step {
    #[must_use]
    pub fn returns(command: QmpCommand, value: Value) -> Self {
        Self::Reply {
            command,
            answer: Answer::Return(value),
        }
    }

    #[must_use]
    pub fn refuses(command: QmpCommand, desc: &str) -> Self {
        Self::Reply {
            command,
            answer: Answer::Refuse(desc.to_owned()),
        }
    }

    #[must_use]
    pub fn raw(command: QmpCommand, line: String) -> Self {
        Self::Reply {
            command,
            answer: Answer::Raw(line),
        }
    }
}

#[must_use]
pub fn devices(entries: &[(&str, &str)]) -> Value {
    Value::Array(
        entries
            .iter()
            .map(|(id, kind)| {
                json!({
                    "type": kind,
                    "data": { "id": id, "size": 0, "requested-size": 0, "block-size": 2_097_152 },
                })
            })
            .collect(),
    )
}

#[must_use]
pub fn summary(base: u64) -> Value {
    json!({ "base-memory": base, "plugged-memory": 0 })
}

#[must_use]
pub fn resized(device: &str, size: u64) -> String {
    json!({
        "timestamp": { "seconds": 1_700_000_000, "microseconds": 0 },
        "event": "MEMORY_DEVICE_SIZE_CHANGE",
        "data": { "id": device, "size": size, "qom-path": qom_path(device) },
    })
    .to_string()
}

#[must_use]
pub fn resized_anonymously(qom: &str, size: u64) -> String {
    json!({
        "timestamp": { "seconds": 1_700_000_000, "microseconds": 0 },
        "event": "MEMORY_DEVICE_SIZE_CHANGE",
        "data": { "size": size, "qom-path": qom },
    })
    .to_string()
}

#[must_use]
pub fn verified(device: &str, boot: u64) -> Vec<Step> {
    vec![
        Step::returns(
            QmpCommand::QueryMemoryDevices,
            devices(&[(device, "virtio-mem")]),
        ),
        Step::returns(QmpCommand::QueryMemorySizeSummary, summary(boot)),
    ]
}

#[must_use]
pub fn startup(device: &str, boot: u64, size: u64) -> Vec<Step> {
    let mut steps = verified(device, boot);
    steps.push(Step::returns(QmpCommand::QomGet, json!(size)));

    steps
}

/// [`startup`] for a socket shared with other tools, which reads the request too.
#[must_use]
pub fn shared(device: &str, boot: u64, size: u64, requested: u64) -> Vec<Step> {
    let mut steps = startup(device, boot, size);
    steps.push(Step::returns(QmpCommand::QomGet, json!(requested)));

    steps
}

#[derive(Debug, Default)]
struct Log {
    asks: Vec<Ask>,
    connections: usize,
}

#[derive(Debug)]
pub struct FakeQmp {
    path: PathBuf,
    log: Arc<Mutex<Log>>,
    task: JoinHandle<()>,

    #[expect(dead_code, reason = "the socket lives as long as the fixture")]
    dir: TempDir,
}

impl FakeQmp {
    pub fn start(script: impl IntoIterator<Item = Vec<Step>>) -> Self {
        let dir = TempDir::new().expect("a temporary directory");
        let path = dir.path().join("qmp.sock");

        let listener = UnixListener::bind(&path).expect("a bound QMP socket");
        let log = Arc::new(Mutex::new(Log::default()));

        let task = tokio::spawn(accept(
            listener,
            script.into_iter().collect(),
            Arc::clone(&log),
        ));

        Self {
            path,
            log,
            task,
            dir,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn commands(&self) -> Vec<QmpCommand> {
        self.locked()
            .asks
            .iter()
            .map(|ask| {
                COMMANDS
                    .into_iter()
                    .find(|command| command.as_str() == ask.execute)
                    .expect("a command the daemon sends")
            })
            .collect()
    }

    pub fn arguments(&self, command: QmpCommand) -> Vec<Value> {
        self.locked()
            .asks
            .iter()
            .filter(|ask| ask.execute == command.as_str())
            .map(|ask| ask.arguments.clone())
            .collect()
    }

    pub fn connections(&self) -> usize {
        self.locked().connections
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, Log> {
        self.log.lock().expect("no test poisons the fake")
    }
}

impl Drop for FakeQmp {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Debug, Clone, Deserialize)]
struct Ask {
    execute: String,
    id: u64,

    #[serde(default)]
    arguments: Value,
}

async fn accept(listener: UnixListener, script: Vec<Vec<Step>>, log: Arc<Mutex<Log>>) {
    for steps in script {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };

        log.lock().expect("no test poisons the fake").connections += 1;

        let _ = serve(stream, steps, &log).await;
    }
}

async fn serve(stream: UnixStream, steps: Vec<Step>, log: &Mutex<Log>) -> io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    send(&mut writer, GREETING).await?;

    let scripted = matches!(
        steps.first(),
        Some(Step::Reply { command, .. }) if *command == QmpCommand::Capabilities
    );

    if !scripted {
        let Some(ask) = next(&mut lines, log).await? else {
            return Ok(());
        };

        let answer = if ask.execute == QmpCommand::Capabilities.as_str() {
            json!({ "return": {}, "id": ask.id })
        } else {
            refusal(
                &format!("expected {}, got {}", QmpCommand::Capabilities, ask.execute),
                ask.id,
            )
        };

        send(&mut writer, &answer.to_string()).await?;
    }

    for step in steps {
        match step {
            Step::Reply { command, answer } => {
                let Some(ask) = next(&mut lines, log).await? else {
                    return Ok(());
                };

                let line = if ask.execute == command.as_str() {
                    match answer {
                        Answer::Return(value) => {
                            json!({ "return": value, "id": ask.id }).to_string()
                        }
                        Answer::Refuse(desc) => refusal(&desc, ask.id).to_string(),
                        Answer::Raw(line) => line,
                    }
                } else {
                    refusal(&format!("expected {command}, got {}", ask.execute), ask.id).to_string()
                };

                send(&mut writer, &line).await?;
            }
            Step::Emit(line) => send(&mut writer, &line).await?,
            Step::Ignore => {
                if next(&mut lines, log).await?.is_none() {
                    return Ok(());
                }
            }
            Step::Hangup => return Ok(()),
        }
    }

    next(&mut lines, log).await?;

    Ok(())
}

async fn next(
    lines: &mut Lines<BufReader<OwnedReadHalf>>,
    log: &Mutex<Log>,
) -> io::Result<Option<Ask>> {
    let Some(line) = lines.next_line().await? else {
        return Ok(None);
    };

    let ask: Ask = serde_json::from_str(&line).expect("a QMP command");

    log.lock()
        .expect("no test poisons the fake")
        .asks
        .push(ask.clone());

    Ok(Some(ask))
}

async fn send(writer: &mut OwnedWriteHalf, line: &str) -> io::Result<()> {
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await
}

fn refusal(desc: &str, id: u64) -> Value {
    json!({ "error": { "class": "GenericError", "desc": desc }, "id": id })
}
