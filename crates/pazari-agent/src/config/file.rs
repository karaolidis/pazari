#![expect(
    clippy::empty_enums,
    reason = "the field enum serde derives for a variant that takes no keys"
)]

use std::{
    net::SocketAddr,
    num::{NonZeroU32, NonZeroU64},
    path::PathBuf,
    time::Duration,
};

use bytesize::ByteSize;
use doku::Document;
use pazari_config::{ConfigError, Positive, Ranged, ordered};
use pazari_proto::DEFAULT_VSOCK_PORT;
use pazari_transport::{
    AnyConnector, UnixConnector, VsockConnector, adapter::vsock::VMADDR_CID_HOST,
};
use pazari_types::{Delay, Interval, Psi, Swappiness};
use serde::Deserialize;
use serde_inline_default::serde_inline_default;

use crate::{
    config::Config,
    engine::{EngineConfig, ExhaustionMode, IdleThreshold},
    runtime::{Backend, RuntimeConfig},
    session::SessionConfig,
};

const STALL_TIMEOUT: Duration = Duration::from_secs(60);

/// pazari-agent configuration.
#[serde_inline_default]
#[derive(Debug, Deserialize, Document)]
#[serde(deny_unknown_fields)]
pub struct File {
    /// How often the agent reads the workload cgroup and `/proc/meminfo`.
    #[serde_inline_default(Duration::from_secs(1))]
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[doku(as = "String", example = "1s")]
    tick_interval: Duration,

    engine: EngineSection,

    runtime: RuntimeSection,

    #[serde(default)]
    metrics: Option<MetricsSection>,

    #[doku(example = "kind = \"vsock\"")]
    transport: TransportSection,
}

impl File {
    pub fn resolve(self) -> Result<Config, ConfigError> {
        let tick = Interval::try_new(self.tick_interval).at("tick_interval")?;

        Ok(Config {
            engine: self.engine.resolve(tick)?,
            runtime: self.runtime.resolve(tick)?,
            session: self.transport.resolve()?,
            metrics: self.metrics.map(|metrics| metrics.listen),
        })
    }
}

#[derive(Debug, Deserialize, Document)]
#[serde(deny_unknown_fields)]
pub struct MetricsSection {
    /// The address the Prometheus endpoint answers scrapes on.
    #[doku(as = "String", example = "127.0.0.1:9651")]
    listen: SocketAddr,
}

#[serde_inline_default]
#[derive(Debug, Deserialize, Document)]
#[serde(deny_unknown_fields)]
pub struct EngineSection {
    /// Memory withheld for the guest's own services, excluding the workload.
    /// Set me.
    #[doku(as = "String", example = "256MiB")]
    service_reserve: ByteSize,

    /// The standing headroom the brake releases when the workload is throttled,
    /// i.e. a burst the workload can take while waiting for more memory from
    /// the host. Set me.
    #[doku(as = "String", example = "256MiB")]
    burst_reserve: ByteSize,

    /// The sliver of the burst reserve the brake holds back while a grant is in
    /// flight to stop an overshoot. Set me.
    #[doku(as = "String", example = "64MiB")]
    open_guard: ByteSize,

    /// How much more memory a grant asks for.
    #[serde_inline_default(ByteSize::mib(512))]
    #[doku(as = "String", example = "512MiB")]
    grow_step: ByteSize,

    /// How long to wait before re-asking a host that denied a request.
    #[serde_inline_default(Duration::from_secs(5))]
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[doku(as = "String", example = "5s")]
    deny_backoff: Duration,

    /// How long a resize is left to settle.
    #[serde_inline_default(Duration::from_secs(10))]
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[doku(as = "String", example = "10s")]
    settle_grace: Duration,

    /// The share of its budget the workload must stay under to count as idle.
    #[serde_inline_default(0.5)]
    #[doku(example = "0.5")]
    idle_threshold: f64,

    /// How many consecutive idle intervals it takes before the agent offers
    /// memory back.
    #[serde_inline_default(3)]
    #[doku(example = "3")]
    idle_intervals: u32,

    /// How much an offer to release gives back at a time.
    #[serde_inline_default(ByteSize::mib(256))]
    #[doku(as = "String", example = "256MiB")]
    shrink_step: ByteSize,

    /// How long after a release before another one is allowed.
    #[serde_inline_default(Duration::from_secs(30))]
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[doku(as = "String", example = "30s")]
    shrink_cooldown: Duration,

    /// The `swappiness=` argument for `memory.reclaim`. Leave the key out to
    /// reclaim at the guest's own `vm.swappiness`.
    #[serde_inline_default(None)]
    #[doku(example = "0")]
    reclaim_swappiness: Option<u8>,

    /// How much memory one titration step claims while the agent is still
    /// finding the workload's working set.
    #[serde_inline_default(ByteSize::mib(64))]
    #[doku(as = "String", example = "64MiB")]
    titrate_step: ByteSize,

    /// How long each titration step is given before the next one.
    #[serde_inline_default(Duration::from_secs(10))]
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[doku(as = "String", example = "10s")]
    titrate_interval: Duration,

    /// The memory pressure, in percent of wall time, at which titration decides
    /// it has found the edge of the working set.
    #[serde_inline_default(5.0)]
    #[doku(example = "5.0")]
    titrate_psi_threshold: f64,

    /// What the agent does once the workload wants more than the ceiling
    /// allows.
    #[serde_inline_default(ExhaustionSection::Stall { timeout: STALL_TIMEOUT })]
    #[doku(example = "kind = \"stall\"")]
    exhaustion: ExhaustionSection,
}

impl EngineSection {
    fn resolve(self, tick: Interval) -> Result<EngineConfig, ConfigError> {
        Ok(EngineConfig {
            service_reserve: self.service_reserve.as_u64(),
            burst_reserve: NonZeroU64::new(self.burst_reserve.as_u64())
                .bytes("engine.burst_reserve")?,
            open_guard: NonZeroU64::new(self.open_guard.as_u64()).bytes("engine.open_guard")?,

            grow_step: NonZeroU64::new(self.grow_step.as_u64()).bytes("engine.grow_step")?,
            deny_backoff: Interval::try_new(self.deny_backoff).at("engine.deny_backoff")?,
            settle_grace: Interval::try_new(self.settle_grace).at("engine.settle_grace")?,

            idle_threshold: IdleThreshold::try_new(self.idle_threshold)
                .at("engine.idle_threshold")?,
            idle_intervals: NonZeroU32::new(self.idle_intervals)
                .positive("engine.idle_intervals", "count of intervals")?,

            shrink_step: NonZeroU64::new(self.shrink_step.as_u64()).bytes("engine.shrink_step")?,
            shrink_cooldown: Delay::try_new(self.shrink_cooldown).at("engine.shrink_cooldown")?,

            reclaim_swappiness: self
                .reclaim_swappiness
                .map(Swappiness::try_new)
                .transpose()
                .at("engine.reclaim_swappiness")?,

            titrate_step: NonZeroU64::new(self.titrate_step.as_u64())
                .bytes("engine.titrate_step")?,
            titrate_interval: Interval::try_new(self.titrate_interval)
                .at("engine.titrate_interval")?,
            titrate_psi_threshold: Psi::try_new(self.titrate_psi_threshold)
                .at("engine.titrate_psi_threshold")?,

            exhaustion: self.exhaustion.resolve()?,

            tick_interval: tick,
        })
    }
}

/// What the agent does once the workload wants more than the ceiling allows.
#[serde_inline_default]
#[derive(Debug, Deserialize, Document)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExhaustionSection {
    /// Leave the band open and let the workload degrade.
    Stall {
        /// How long a workload may want more than the ceiling allows before the
        /// agent stops waiting for the host to move.
        #[serde_inline_default(STALL_TIMEOUT)]
        #[serde(deserialize_with = "humantime_serde::deserialize")]
        #[doku(as = "String", example = "60s")]
        timeout: Duration,
    },

    /// Let the workload reach `memory.max` and be killed as a unit.
    Kill {},
}

impl ExhaustionSection {
    fn resolve(self) -> Result<ExhaustionMode, ConfigError> {
        match self {
            Self::Stall { timeout } => Ok(ExhaustionMode::Stall {
                timeout: Interval::try_new(timeout).at("engine.exhaustion.timeout")?,
            }),
            Self::Kill {} => Ok(ExhaustionMode::Kill),
        }
    }
}

#[serde_inline_default]
#[derive(Debug, Deserialize, Document)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSection {
    /// The managed workload cgroup, as an absolute path under the unified
    /// hierarchy. Set me.
    #[doku(example = "/sys/fs/cgroup/workload.slice")]
    cgroup: PathBuf,

    /// What the guest booted with, excluding anything hot-plugged. Set me.
    #[doku(as = "String", example = "2GiB")]
    boot_mem: ByteSize,

    /// How long the hotplug listener waits for one resize's uevent to end.
    #[serde_inline_default(Duration::from_millis(250))]
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[doku(as = "String", example = "250ms")]
    hotplug_debounce: Duration,

    /// How much one `memory.reclaim` write asks for.
    #[serde_inline_default(ByteSize::mib(64))]
    #[doku(as = "String", example = "64MiB")]
    reclaim_chunk: ByteSize,

    /// Whether `/sys/kernel/mm/transparent_hugepage/enabled` set to `always`
    /// fails preflight.
    #[serde_inline_default(true)]
    #[doku(example = "true")]
    thp_always_fatal: bool,

    /// Which adapter writes `memory.high` and `memory.max`.
    #[serde_inline_default(BackendSection::Cgroupfs {})]
    #[doku(example = "kind = \"cgroupfs\"")]
    backend: BackendSection,
}

impl RuntimeSection {
    fn resolve(self, tick: Interval) -> Result<RuntimeConfig, ConfigError> {
        Ok(RuntimeConfig {
            backend: self.backend.resolve(),
            tick_interval: tick,
            hotplug_debounce: Interval::try_new(self.hotplug_debounce)
                .at("runtime.hotplug_debounce")?,
            reclaim_chunk: NonZeroU64::new(self.reclaim_chunk.as_u64())
                .bytes("runtime.reclaim_chunk")?,
            thp_always_fatal: self.thp_always_fatal,
            ..RuntimeConfig::new(
                self.cgroup,
                NonZeroU64::new(self.boot_mem.as_u64()).bytes("runtime.boot_mem")?,
            )
        })
    }
}

/// Which adapter writes `memory.high` and `memory.max`.
#[derive(Debug, Deserialize, Document)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackendSection {
    /// Straight to the cgroup's own files.
    Cgroupfs {},

    /// Through the runtime properties of a unit.
    Systemd {
        /// The unit carrying the limits as `MemoryHigh` and `MemoryMax`. Set me.
        #[doku(example = "workload.service")]
        unit: String,
    },
}

impl BackendSection {
    fn resolve(self) -> Backend {
        match self {
            Self::Cgroupfs {} => Backend::Cgroupfs,
            Self::Systemd { unit } => Backend::Systemd { unit },
        }
    }
}

/// How the agent reaches the host daemon.
#[serde_inline_default]
#[derive(Debug, Deserialize, Document)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TransportSection {
    /// `AF_VSOCK`.
    Vsock {
        /// The host's context id.
        #[serde_inline_default(VMADDR_CID_HOST)]
        #[doku(example = "2")]
        cid: u32,

        /// The port pazarid listens on.
        #[serde_inline_default(DEFAULT_VSOCK_PORT)]
        #[doku(example = "4650")]
        port: u32,

        /// How long to wait for the host's `welcome`.
        #[serde_inline_default(Duration::from_secs(10))]
        #[serde(deserialize_with = "humantime_serde::deserialize")]
        #[doku(as = "String", example = "10s")]
        handshake_timeout: Duration,

        /// The first reconnect delay.
        #[serde_inline_default(Duration::from_millis(500))]
        #[serde(deserialize_with = "humantime_serde::deserialize")]
        #[doku(as = "String", example = "500ms")]
        reconnect_min: Duration,

        /// The cap the reconnect delay doubles towards.
        #[serde_inline_default(Duration::from_secs(30))]
        #[serde(deserialize_with = "humantime_serde::deserialize")]
        #[doku(as = "String", example = "30s")]
        reconnect_max: Duration,
    },

    /// An `AF_UNIX` socket.
    Unix {
        /// The socket pazarid listens on. Set me.
        #[doku(example = "/run/pazari/host.sock")]
        path: PathBuf,

        /// How long to wait for the host's `welcome`.
        #[serde_inline_default(Duration::from_secs(10))]
        #[serde(deserialize_with = "humantime_serde::deserialize")]
        #[doku(as = "String", example = "10s")]
        handshake_timeout: Duration,

        /// The first reconnect delay.
        #[serde_inline_default(Duration::from_millis(500))]
        #[serde(deserialize_with = "humantime_serde::deserialize")]
        #[doku(as = "String", example = "500ms")]
        reconnect_min: Duration,

        /// The cap the reconnect delay doubles towards.
        #[serde_inline_default(Duration::from_secs(30))]
        #[serde(deserialize_with = "humantime_serde::deserialize")]
        #[doku(as = "String", example = "30s")]
        reconnect_max: Duration,
    },
}

impl TransportSection {
    fn resolve(self) -> Result<SessionConfig, ConfigError> {
        let (connector, handshake_timeout, reconnect_min, reconnect_max) = match self {
            Self::Vsock {
                cid,
                port,
                handshake_timeout,
                reconnect_min,
                reconnect_max,
            } => (
                AnyConnector::Vsock(VsockConnector::new(cid, port)),
                handshake_timeout,
                reconnect_min,
                reconnect_max,
            ),
            Self::Unix {
                path,
                handshake_timeout,
                reconnect_min,
                reconnect_max,
            } => (
                AnyConnector::Unix(UnixConnector::new(path)),
                handshake_timeout,
                reconnect_min,
                reconnect_max,
            ),
        };

        let reconnect_min = Interval::try_new(reconnect_min).at("transport.reconnect_min")?;
        let reconnect_max = Interval::try_new(reconnect_max).at("transport.reconnect_max")?;

        ordered(
            "transport.reconnect_min",
            reconnect_min,
            "transport.reconnect_max",
            reconnect_max,
        )?;

        Ok(SessionConfig {
            connector,
            handshake_timeout: Interval::try_new(handshake_timeout)
                .at("transport.handshake_timeout")?,
            reconnect_min,
            reconnect_max,
        })
    }
}
