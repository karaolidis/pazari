#![expect(
    clippy::empty_enums,
    reason = "the field enum serde derives for a variant that takes no keys"
)]

use std::{
    collections::{BTreeMap, BTreeSet},
    num::{NonZeroU32, NonZeroU64},
    path::PathBuf,
    time::Duration,
};

use bytesize::ByteSize;
use doku::Document;
use pazari_proto::DEFAULT_VSOCK_PORT;
use pazari_transport::{PeerId, adapter::vsock::VMADDR_CID_ANY};
use pazari_types::{Interval, Psi};
use serde::Deserialize;
use serde_inline_default::serde_inline_default;

use crate::{
    broker::{GuestSpec, Policy, Registry, Revocation},
    config::{Config, Qmp, Transport, error::ConfigError},
    runtime::Mode,
};

const RETRY_MIN: Duration = Duration::from_millis(500);
const RETRY_MAX: Duration = Duration::from_secs(30);

#[serde_inline_default]
#[derive(Debug, Deserialize, Document)]
#[serde(deny_unknown_fields)]
pub struct File {
    /// How often the daemon reads the host's memory and updates what it knows
    /// about its guests.
    #[serde_inline_default(Duration::from_secs(1))]
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[doku(as = "String", example = "1s")]
    tick_interval: Duration,

    policy: PolicySection,

    #[serde(default)]
    #[doku(as = "GuestsTemplate")]
    guests: BTreeMap<String, GuestSection>,
}

impl File {
    pub fn resolve(self) -> Result<Config, ConfigError> {
        let tick_interval =
            Interval::try_new(self.tick_interval).map_err(|source| ConfigError::Range {
                field: "tick_interval".to_owned(),
                source,
            })?;

        let policy = self.policy.resolve()?;

        if self.guests.is_empty() {
            return Err(ConfigError::Guestless);
        }

        let mut registry = Registry::new();
        let mut devices = Vec::with_capacity(self.guests.len());
        let mut ports = BTreeSet::new();
        let mut sockets = Vec::new();

        for (name, guest) in self.guests {
            let prefix = format!("guests.{name}");

            let GuestSection {
                ceiling,
                transport,
                qmp,
            } = guest;

            let peer = match transport {
                TransportSection::Vsock { cid, port } => {
                    ports.insert(port);
                    PeerId::Vsock { cid }
                }
                TransportSection::Unix { path } => {
                    sockets.push(Transport::Unix { path: path.clone() });
                    PeerId::Unix { path }
                }
            };

            registry.insert(
                peer,
                GuestSpec {
                    name,
                    ceiling: ceiling.as_u64(),
                },
            )?;

            devices.push(qmp.resolve(&prefix)?);
        }

        let listeners = ports
            .into_iter()
            .map(|port| Transport::Vsock {
                cid: VMADDR_CID_ANY,
                port,
            })
            .chain(sockets)
            .collect();

        Ok(Config {
            tick_interval,
            listeners,
            policy,
            registry,
            devices,
        })
    }
}

#[serde_inline_default]
#[derive(Debug, Deserialize, Document)]
#[serde(deny_unknown_fields)]
pub struct PolicySection {
    /// How often a guest must send `status`.
    #[serde_inline_default(Duration::from_secs(1))]
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[doku(as = "String", example = "1s")]
    welcome_status_interval: Duration,

    /// Memory the host keeps for itself. No grow is granted that would take
    /// `MemAvailable` under it. Set me.
    #[doku(as = "String", example = "2GiB")]
    host_floor: ByteSize,

    /// How long a guest waits after a grant before another one.
    #[serde_inline_default(Duration::from_secs(5))]
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[doku(as = "String", example = "5s")]
    grant_min_interval: Duration,

    /// How long an actuation is given to apply before the host stops waiting
    /// for it.
    #[serde_inline_default(Duration::from_secs(30))]
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[doku(as = "String", example = "30s")]
    reconcile_deadline: Duration,

    /// Whether a shrink that did not apply is taken back to the size the guest
    /// still has.
    #[serde_inline_default(true)]
    #[doku(example = "true")]
    shrink_revert: bool,

    /// Missed `status` intervals before a guest is marked stale.
    #[serde_inline_default(3)]
    #[doku(example = "3")]
    stale_after: u32,

    /// How long a guest has to answer a `reclaim_request`.
    #[serde_inline_default(Duration::from_secs(30))]
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[doku(as = "String", example = "30s")]
    reclaim_deadline: Duration,

    /// Whether free host swap counts towards what a grow may be granted out
    /// of.
    #[serde_inline_default(false)]
    #[doku(example = "false")]
    grant_swap: bool,

    /// How the host takes memory back when it comes under pressure itself.
    #[serde(default)]
    revocation: Option<RevocationSection>,
}

impl PolicySection {
    fn resolve(self) -> Result<Policy, ConfigError> {
        Ok(Policy {
            welcome_status_interval: Interval::try_new(self.welcome_status_interval).map_err(
                |source| ConfigError::Range {
                    field: "policy.welcome_status_interval".to_owned(),
                    source,
                },
            )?,
            host_floor: self.host_floor.as_u64(),
            grant_min_interval: Interval::try_new(self.grant_min_interval).map_err(|source| {
                ConfigError::Range {
                    field: "policy.grant_min_interval".to_owned(),
                    source,
                }
            })?,
            reconcile_deadline: Interval::try_new(self.reconcile_deadline).map_err(|source| {
                ConfigError::Range {
                    field: "policy.reconcile_deadline".to_owned(),
                    source,
                }
            })?,
            shrink_revert: self.shrink_revert,
            stale_after: NonZeroU32::new(self.stale_after).ok_or_else(|| ConfigError::Zero {
                field: "policy.stale_after".to_owned(),
                what: "count of intervals",
            })?,
            reclaim_deadline: Interval::try_new(self.reclaim_deadline).map_err(|source| {
                ConfigError::Range {
                    field: "policy.reclaim_deadline".to_owned(),
                    source,
                }
            })?,
            grant_swap: self.grant_swap,
            revocation: self
                .revocation
                .map(RevocationSection::resolve)
                .transpose()?,
        })
    }
}

/// How the host takes memory back when it comes under pressure itself.
#[serde_inline_default]
#[derive(Debug, Deserialize, Document)]
#[serde(deny_unknown_fields)]
pub struct RevocationSection {
    /// The host memory pressure, in percent of wall time, at which the daemon
    /// starts asking guests for memory back. Set me.
    #[doku(example = "20.0")]
    psi: f64,

    /// How much memory one such request asks a guest for.
    #[serde_inline_default(ByteSize::mib(256))]
    #[doku(as = "String", example = "256MiB")]
    step: ByteSize,
}

impl RevocationSection {
    fn resolve(self) -> Result<Revocation, ConfigError> {
        Ok(Revocation {
            psi: Psi::try_new(self.psi).map_err(|source| ConfigError::Range {
                field: "policy.revocation.psi".to_owned(),
                source,
            })?,
            step: NonZeroU64::new(self.step.as_u64()).ok_or_else(|| ConfigError::Zero {
                field: "policy.revocation.step".to_owned(),
                what: "byte count",
            })?,
        })
    }
}

/// Stands in for the guest tables which doku will not print off a map.
#[derive(Deserialize, Document)]
struct GuestsTemplate {
    /// The guests the daemon answers to, one table each.
    #[expect(dead_code, reason = "the field only shapes the printed template")]
    example: GuestSection,
}

#[serde_inline_default]
#[derive(Debug, Deserialize, Document)]
#[serde(deny_unknown_fields)]
pub struct GuestSection {
    /// The most memory this guest may hold. Set me.
    #[doku(as = "String", example = "8GiB")]
    ceiling: ByteSize,

    /// How this guest reaches the daemon.
    // doku sketches a commented enum as its tag alone, so the required cid
    // rides along in the example to keep the printed template parseable.
    #[doku(example = "kind = \"vsock\"\ncid = 3")]
    transport: TransportSection,

    qmp: QmpSection,
}

#[serde_inline_default]
#[derive(Debug, Deserialize, Document)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TransportSection {
    /// `AF_VSOCK`.
    Vsock {
        /// The context id the guest dials from. Set me.
        #[doku(example = "3")]
        cid: u32,

        /// The port the guest dials.
        #[serde_inline_default(DEFAULT_VSOCK_PORT)]
        #[doku(example = "4650")]
        port: u32,
    },

    /// An `AF_UNIX` socket.
    Unix {
        /// The socket this guest dials. Set me.
        #[doku(example = "/run/pazari/example.sock")]
        path: PathBuf,
    },
}

#[serde_inline_default]
#[derive(Debug, Deserialize, Document)]
#[serde(deny_unknown_fields)]
pub struct QmpSection {
    /// The QMP socket of the qemu process running the guest. Set me.
    #[doku(example = "/run/pazari/example.qmp")]
    path: PathBuf,

    /// The `id=` of the guest's virtio-mem device. Set me.
    #[doku(example = "vm0")]
    device: String,

    /// How long one QMP command is given to finish.
    #[serde_inline_default(Duration::from_secs(5))]
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[doku(as = "String", example = "5s")]
    deadline: Duration,

    /// The first delay before a device that did not answer is opened again.
    #[serde_inline_default(RETRY_MIN)]
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[doku(as = "String", example = "500ms")]
    retry_min: Duration,

    /// The cap that delay doubles towards.
    #[serde_inline_default(RETRY_MAX)]
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[doku(as = "String", example = "30s")]
    retry_max: Duration,

    /// How the daemon uses the QMP socket.
    #[serde_inline_default(ModeSection::Held {})]
    #[doku(example = "kind = \"held\"")]
    mode: ModeSection,
}

impl QmpSection {
    fn resolve(self, prefix: &str) -> Result<Qmp, ConfigError> {
        let retry_min = Interval::try_new(self.retry_min).map_err(|source| ConfigError::Range {
            field: format!("{prefix}.qmp.retry_min"),
            source,
        })?;
        let retry_max = Interval::try_new(self.retry_max).map_err(|source| ConfigError::Range {
            field: format!("{prefix}.qmp.retry_max"),
            source,
        })?;

        if retry_min > retry_max {
            return Err(ConfigError::order(
                format!("{prefix}.qmp.retry_min"),
                &retry_min.get(),
                format!("{prefix}.qmp.retry_max"),
                &retry_max.get(),
            ));
        }

        Ok(Qmp {
            path: self.path,
            device: self.device,
            mode: self.mode.resolve(prefix)?,
            deadline: Interval::try_new(self.deadline).map_err(|source| ConfigError::Range {
                field: format!("{prefix}.qmp.deadline"),
                source,
            })?,
            retry_min,
            retry_max,
        })
    }
}

#[serde_inline_default]
#[derive(Debug, Deserialize, Document)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModeSection {
    /// One connection held open.
    Held {},

    /// A fresh connection per command, for a socket shared with other tools.
    PerOperation {
        /// How often the size is resampled while a resize is being processed.
        #[serde_inline_default(Duration::from_secs(1))]
        #[serde(deserialize_with = "humantime_serde::deserialize")]
        #[doku(as = "String", example = "1s")]
        poll: Duration,
    },
}

impl ModeSection {
    fn resolve(self, prefix: &str) -> Result<Mode, ConfigError> {
        match self {
            Self::Held {} => Ok(Mode::Held),
            Self::PerOperation { poll } => Ok(Mode::PerOperation {
                poll: Interval::try_new(poll).map_err(|source| ConfigError::Range {
                    field: format!("{prefix}.qmp.mode.poll"),
                    source,
                })?,
            }),
        }
    }
}
