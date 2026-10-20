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
use pazari_config::{Absolute, ConfigError, Positive, Ranged, ordered};
use pazari_transport::{
    AnyConnector, UnixConnector, VsockConnector,
    adapter::vsock::{DEFAULT_VSOCK_PORT, VMADDR_CID_HOST},
};
use pazari_types::{Delay, Interval, Swappiness};
use serde::Deserialize;
use serde_inline_default::serde_inline_default;

use crate::{
    config::Config,
    engine::{
        EngineConfig, ExhaustionMode, ExponentialConfig, GrowConfig, ReleaseConfig, SweepConfig,
        ThrottleMode, TitrateConfig,
    },
    runtime::{Backend, Paths, ReclaimConfig, RuntimeConfig, ThpAlways},
    session::SessionConfig,
};

const TICK_INTERVAL: Duration = Duration::from_secs(1);

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const RECONNECT_MIN: Duration = Duration::from_millis(500);
const RECONNECT_MAX: Duration = Duration::from_secs(30);

const SETTLE_GRACE: Duration = Duration::from_secs(10);
const DENY_BACKOFF: Duration = Duration::from_secs(5);
const HOTPLUG_DEBOUNCE: Duration = Duration::from_millis(250);

/// 64 MiB: large enough that releasing a few hundred megabytes is a handful of
/// writes, small enough that a cancelled reclaim stops inside one of them.
const RECLAIM_CHUNK: ByteSize = ByteSize::mib(64);

const THROTTLE_RECLAIM: ByteSize = ByteSize::mib(256);

const GROW_STEP: ByteSize = ByteSize::mib(512);
const ESCALATE_WITHIN: Duration = Duration::from_secs(10);
const GROW_CAP: u32 = 64;

const RELEASE_STEP: ByteSize = ByteSize::mib(256);
const RELEASE_AFTER: u32 = 3;
const RELEASE_COOLDOWN: Duration = Duration::from_secs(30);

const SWEEP_MIN: ByteSize = ByteSize::mib(256);
const SWEEP_HOLD: Duration = Duration::from_mins(5);

const RECOVER_AFTER: u32 = 3;
const STALL_TIMEOUT: Duration = Duration::from_secs(60);

const THP_ALWAYS: ThpAlwaysSection = ThpAlwaysSection::Fail;

/// pazari-agent configuration.
#[serde_inline_default]
#[derive(Debug, Deserialize, Document)]
#[serde(deny_unknown_fields)]
pub struct File {
    #[serde(default)]
    tick: TickSection,

    workload: WorkloadSection,

    host: HostSection,

    #[serde(default)]
    resize: ResizeSection,

    #[serde(default)]
    reclaim: ReclaimSection,

    /// What the agent does when the workload throttles at `memory.high`.
    #[serde_inline_default(ThrottleSection::Grow {})]
    #[doku(example = "kind = \"grow\"")]
    throttle: ThrottleSection,

    /// How much more memory each request asks the host for.
    #[serde_inline_default(GrowSection::Exponential {
        step: GROW_STEP,
        escalate_within: ESCALATE_WITHIN,
        cap: GROW_CAP,
    })]
    #[doku(example = "kind = \"exponential\"")]
    grow: GrowSection,

    /// When the guest gives memory back to the host, and how much.
    #[serde_inline_default(ReleaseSection::Titrate {
        step: RELEASE_STEP,
        after: RELEASE_AFTER,
        cooldown: RELEASE_COOLDOWN,
    })]
    #[doku(example = "kind = \"titrate\"")]
    release: ReleaseSection,

    #[serde(default)]
    exhaustion: ExhaustionSection,

    #[serde(default)]
    preflight: PreflightSection,

    metrics: Option<MetricsSection>,
}

impl File {
    pub fn resolve(self) -> Result<Config, ConfigError> {
        let ExhaustionSection {
            recover_after,
            mode,
        } = self.exhaustion;

        let engine = EngineConfig {
            service_reserve: self.workload.service_reserve.as_u64(),
            burst_reserve: NonZeroU64::new(self.workload.burst_reserve.as_u64())
                .bytes("workload.burst_reserve")?,

            throttle: self.throttle.resolve()?,
            grow: self.grow.resolve()?,
            release: self.release.resolve()?,

            settle_grace: Interval::try_new(self.resize.settle_grace).at("resize.settle_grace")?,
            deny_backoff: Interval::try_new(self.resize.deny_backoff).at("resize.deny_backoff")?,
            recover_after: NonZeroU32::new(recover_after)
                .positive("exhaustion.recover_after", "count of intervals")?,

            exhaustion: mode.resolve()?,
        };

        let runtime = RuntimeConfig {
            tick_interval: Interval::try_new(self.tick.interval).at("tick.interval")?,

            cgroup: self.workload.cgroup.absolute("workload.cgroup")?,
            backend: self.workload.backend.resolve(),

            hotplug_debounce: Interval::try_new(self.resize.hotplug_debounce)
                .at("resize.hotplug_debounce")?,

            reclaim: self.reclaim.resolve()?,

            thp_always: self.preflight.thp_always.resolve(),

            paths: Paths::default(),
        };

        Ok(Config {
            engine,
            runtime,
            session: self.host.resolve()?,
            metrics: self.metrics.map(|metrics| metrics.listen),
        })
    }
}

#[derive(Debug, Deserialize, Document)]
#[serde(default, deny_unknown_fields)]
pub struct TickSection {
    /// How often the agent reads the workload cgroup and `/proc/meminfo`.
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[doku(as = "String", example = "1s")]
    interval: Duration,
}

impl Default for TickSection {
    fn default() -> Self {
        Self {
            interval: TICK_INTERVAL,
        }
    }
}

#[serde_inline_default]
#[derive(Debug, Deserialize, Document)]
#[serde(deny_unknown_fields)]
pub struct WorkloadSection {
    /// The managed workload cgroup, as an absolute path under the unified
    /// hierarchy. Set me.
    #[doku(example = "/sys/fs/cgroup/workload.slice")]
    cgroup: PathBuf,

    /// Memory withheld for the guest's own services, excluding the workload.
    /// Set me.
    #[doku(as = "String", example = "256MiB")]
    service_reserve: ByteSize,

    /// The headroom between `memory.high` and `memory.max`: how far the
    /// workload can overrun the brake before it reaches its hard limit. Set me.
    #[doku(as = "String", example = "256MiB")]
    burst_reserve: ByteSize,

    /// Which adapter writes `memory.high` and `memory.max`.
    #[serde_inline_default(BackendSection::Cgroupfs {})]
    #[doku(example = "kind = \"cgroupfs\"")]
    backend: BackendSection,
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
        #[doku(example = "workload.slice")]
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

/// The session with pazarid.
#[serde_inline_default]
#[derive(Debug, Deserialize, Document)]
#[serde(deny_unknown_fields)]
pub struct HostSection {
    /// How long to wait for the host's `welcome`.
    #[serde_inline_default(HANDSHAKE_TIMEOUT)]
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[doku(as = "String", example = "10s")]
    handshake_timeout: Duration,

    /// The first reconnect delay.
    #[serde_inline_default(RECONNECT_MIN)]
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[doku(as = "String", example = "500ms")]
    reconnect_min: Duration,

    /// The cap the reconnect delay doubles towards.
    #[serde_inline_default(RECONNECT_MAX)]
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[doku(as = "String", example = "30s")]
    reconnect_max: Duration,

    /// How the agent reaches pazarid.
    #[doku(example = "kind = \"vsock\"")]
    transport: TransportSection,
}

impl HostSection {
    fn resolve(self) -> Result<SessionConfig, ConfigError> {
        let reconnect_min = Interval::try_new(self.reconnect_min).at("host.reconnect_min")?;
        let reconnect_max = Interval::try_new(self.reconnect_max).at("host.reconnect_max")?;

        ordered(
            "host.reconnect_min",
            reconnect_min,
            "host.reconnect_max",
            reconnect_max,
        )?;

        Ok(SessionConfig {
            handshake_timeout: Interval::try_new(self.handshake_timeout)
                .at("host.handshake_timeout")?,
            reconnect_min,
            reconnect_max,
            connector: self.transport.resolve(),
        })
    }
}

/// How the agent reaches pazarid.
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
    },

    /// An `AF_UNIX` socket.
    Unix {
        /// The socket pazarid listens on. Set me.
        #[doku(example = "/run/pazari/host.sock")]
        path: PathBuf,
    },
}

impl TransportSection {
    fn resolve(self) -> AnyConnector {
        match self {
            Self::Vsock { cid, port } => AnyConnector::Vsock(VsockConnector::new(cid, port)),
            Self::Unix { path } => AnyConnector::Unix(UnixConnector::new(path)),
        }
    }
}

#[derive(Debug, Deserialize, Document)]
#[serde(default, deny_unknown_fields)]
pub struct ResizeSection {
    /// How long a resize is left to settle.
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[doku(as = "String", example = "10s")]
    settle_grace: Duration,

    /// How long to wait before re-asking a host that denied a request.
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[doku(as = "String", example = "5s")]
    deny_backoff: Duration,

    /// How long the hotplug listener waits for one resize's uevent to end.
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[doku(as = "String", example = "250ms")]
    hotplug_debounce: Duration,
}

impl Default for ResizeSection {
    fn default() -> Self {
        Self {
            settle_grace: SETTLE_GRACE,
            deny_backoff: DENY_BACKOFF,
            hotplug_debounce: HOTPLUG_DEBOUNCE,
        }
    }
}

#[derive(Debug, Deserialize, Document)]
#[serde(default, deny_unknown_fields)]
pub struct ReclaimSection {
    /// How much one `memory.reclaim` write asks for.
    #[doku(as = "String", example = "64MiB")]
    chunk: ByteSize,

    /// The `swappiness=` argument for `memory.reclaim`. Leave the key out to
    /// reclaim at the guest's own `vm.swappiness`.
    #[doku(example = "0")]
    swappiness: Option<u8>,
}

impl Default for ReclaimSection {
    fn default() -> Self {
        Self {
            chunk: RECLAIM_CHUNK,
            swappiness: None,
        }
    }
}

impl ReclaimSection {
    fn resolve(self) -> Result<ReclaimConfig, ConfigError> {
        Ok(ReclaimConfig {
            chunk: NonZeroU64::new(self.chunk.as_u64()).bytes("reclaim.chunk")?,
            swappiness: self
                .swappiness
                .map(Swappiness::try_new)
                .transpose()
                .at("reclaim.swappiness")?,
        })
    }
}

/// What the agent does when the workload throttles at `memory.high`.
#[serde_inline_default]
#[derive(Debug, Deserialize, Document)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ThrottleSection {
    /// Ask the host for more at once.
    Grow {},

    /// Reclaim from the workload before asking the host.
    Reclaim {
        /// How much one reclaim asks for.
        #[serde_inline_default(THROTTLE_RECLAIM)]
        #[doku(as = "String", example = "256MiB")]
        amount: ByteSize,
    },
}

impl ThrottleSection {
    fn resolve(self) -> Result<ThrottleMode, ConfigError> {
        match self {
            Self::Grow {} => Ok(ThrottleMode::Grow),
            Self::Reclaim { amount } => Ok(ThrottleMode::Reclaim {
                amount: NonZeroU64::new(amount.as_u64()).bytes("throttle.amount")?,
            }),
        }
    }
}

/// How much more memory each request asks the host for.
#[serde_inline_default]
#[derive(Debug, Deserialize, Document)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GrowSection {
    /// One step at first. Each throttle that follows soon after the last grow
    /// doubles it, up to `cap` steps.
    Exponential {
        /// The first request, and the unit later ones are multiples of.
        #[serde_inline_default(GROW_STEP)]
        #[doku(as = "String", example = "512MiB")]
        step: ByteSize,

        /// How soon after a grow a throttle still doubles the next request.
        #[serde_inline_default(ESCALATE_WITHIN)]
        #[serde(deserialize_with = "humantime_serde::deserialize")]
        #[doku(as = "String", example = "10s")]
        escalate_within: Duration,

        /// The most steps one request asks for.
        #[serde_inline_default(GROW_CAP)]
        #[doku(example = "64")]
        cap: u32,
    },
}

impl GrowSection {
    fn resolve(self) -> Result<GrowConfig, ConfigError> {
        match self {
            Self::Exponential {
                step,
                escalate_within,
                cap,
            } => Ok(GrowConfig::Exponential(ExponentialConfig {
                step: NonZeroU64::new(step.as_u64()).bytes("grow.step")?,
                escalate_within: Interval::try_new(escalate_within).at("grow.escalate_within")?,
                cap: NonZeroU32::new(cap).positive("grow.cap", "count of steps")?,
            })),
        }
    }
}

/// When the guest gives memory back to the host, and how much.
#[serde_inline_default]
#[derive(Debug, Deserialize, Document)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReleaseSection {
    /// One fixed step at a time. The guest must hold the step spare for a run
    /// of calm intervals. The last resize must have cooled off.
    Titrate {
        /// How much one release gives back.
        #[serde_inline_default(RELEASE_STEP)]
        #[doku(as = "String", example = "256MiB")]
        step: ByteSize,

        /// Intervals of a spare `step` over the reserves before a release.
        #[serde_inline_default(RELEASE_AFTER)]
        #[doku(example = "3")]
        after: u32,

        /// How long after a resize before a release is allowed.
        #[serde_inline_default(RELEASE_COOLDOWN)]
        #[serde(deserialize_with = "humantime_serde::deserialize")]
        #[doku(as = "String", example = "30s")]
        cooldown: Duration,
    },

    /// All the spare at once, once it has held for `hold`.
    Sweep {
        /// The least spare that counts towards the hold.
        #[serde_inline_default(SWEEP_MIN)]
        #[doku(as = "String", example = "256MiB")]
        min: ByteSize,

        /// How long the spare must hold before a release.
        #[serde_inline_default(SWEEP_HOLD)]
        #[serde(deserialize_with = "humantime_serde::deserialize")]
        #[doku(as = "String", example = "5m")]
        hold: Duration,

        /// How long after a resize before a release is allowed.
        #[serde_inline_default(RELEASE_COOLDOWN)]
        #[serde(deserialize_with = "humantime_serde::deserialize")]
        #[doku(as = "String", example = "30s")]
        cooldown: Duration,
    },
}

impl ReleaseSection {
    fn resolve(self) -> Result<ReleaseConfig, ConfigError> {
        match self {
            Self::Titrate {
                step,
                after,
                cooldown,
            } => Ok(ReleaseConfig::Titrate(TitrateConfig {
                step: NonZeroU64::new(step.as_u64()).bytes("release.step")?,
                after: NonZeroU32::new(after).positive("release.after", "count of intervals")?,
                cooldown: Delay::try_new(cooldown).at("release.cooldown")?,
            })),
            Self::Sweep {
                min,
                hold,
                cooldown,
            } => Ok(ReleaseConfig::Sweep(SweepConfig {
                min: NonZeroU64::new(min.as_u64()).bytes("release.min")?,
                hold: Delay::try_new(hold).at("release.hold")?,
                cooldown: Delay::try_new(cooldown).at("release.cooldown")?,
            })),
        }
    }
}

#[derive(Debug, Deserialize, Document)]
#[serde(default, deny_unknown_fields)]
pub struct ExhaustionSection {
    /// Calm intervals that end an exhaustion.
    #[doku(example = "3")]
    recover_after: u32,

    /// What the agent does once the workload wants more than the ceiling
    /// allows.
    #[doku(example = "kind = \"stall\"")]
    mode: ModeSection,
}

impl Default for ExhaustionSection {
    fn default() -> Self {
        Self {
            recover_after: RECOVER_AFTER,
            mode: ModeSection::Stall {
                timeout: STALL_TIMEOUT,
            },
        }
    }
}

/// What the agent does once the workload wants more than the ceiling allows.
#[serde_inline_default]
#[derive(Debug, Deserialize, Document)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModeSection {
    /// Keep the brake on until `timeout`, then release it.
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

impl ModeSection {
    fn resolve(self) -> Result<ExhaustionMode, ConfigError> {
        match self {
            Self::Stall { timeout } => Ok(ExhaustionMode::Stall {
                timeout: Interval::try_new(timeout).at("exhaustion.mode.timeout")?,
            }),
            Self::Kill {} => Ok(ExhaustionMode::Kill),
        }
    }
}

#[derive(Debug, Deserialize, Document)]
#[serde(default, deny_unknown_fields)]
pub struct PreflightSection {
    /// What preflight does when `/sys/kernel/mm/transparent_hugepage/enabled`
    /// is set to `always`.
    #[doku(example = "\"fail\"")]
    thp_always: ThpAlwaysSection,
}

impl Default for PreflightSection {
    fn default() -> Self {
        Self {
            thp_always: THP_ALWAYS,
        }
    }
}

#[derive(Debug, Deserialize, Document)]
#[serde(rename_all = "snake_case")]
pub enum ThpAlwaysSection {
    Fail,
    Warn,
}

impl ThpAlwaysSection {
    const fn resolve(self) -> ThpAlways {
        match self {
            Self::Fail => ThpAlways::Fail,
            Self::Warn => ThpAlways::Warn,
        }
    }
}

#[derive(Debug, Deserialize, Document)]
#[serde(deny_unknown_fields)]
pub struct MetricsSection {
    /// The address to serve `OpenMetrics` on.
    #[doku(as = "String", example = "127.0.0.1:9651")]
    listen: SocketAddr,
}
