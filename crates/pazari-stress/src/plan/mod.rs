use std::{num::NonZeroU64, path::PathBuf, str::FromStr, time::Duration};

use bytesize::ByteSize;
use serde::Serialize;
use serde_with::{DurationMilliSeconds, serde_as};
use thiserror::Error;

pub const DEFAULT_CHUNK: NonZeroU64 = NonZeroU64::new(1 << 20).expect("1MiB is positive");
pub const DEFAULT_RELEASE_CHUNK: NonZeroU64 = NonZeroU64::new(64 << 20).expect("64MiB is positive");

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Phase {
    pub spec: String,

    #[serde(flatten)]
    pub kind: Kind,
}

#[serde_as]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Kind {
    /// Advances the frontier, touching pages that were never touched.
    Grow {
        target_bytes: NonZeroU64,

        #[serde(skip_serializing_if = "Option::is_none")]
        rate_bps: Option<NonZeroU64>,

        chunk: NonZeroU64,
    },

    /// Rewrites the resident set without moving the frontier.
    Churn {
        #[serde_as(as = "DurationMilliSeconds<u64>")]
        #[serde(rename = "target_ms")]
        target: Duration,

        rate_bps: NonZeroU64,

        chunk: NonZeroU64,
    },

    /// Touches nothing.
    Hold {
        #[serde_as(as = "DurationMilliSeconds<u64>")]
        #[serde(rename = "target_ms")]
        target: Duration,
    },

    /// Retreats the frontier, dropping the pages behind it.
    Release {
        target_bytes: NonZeroU64,

        #[serde(skip_serializing_if = "Option::is_none")]
        rate_bps: Option<NonZeroU64>,

        chunk: NonZeroU64,
    },

    /// Grows the page cache without moving the frontier.
    Cache {
        target_bytes: NonZeroU64,

        #[serde(skip_serializing_if = "Option::is_none")]
        rate_bps: Option<NonZeroU64>,

        chunk: NonZeroU64,
    },
}

impl Kind {
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match *self {
            Self::Grow { .. } => Name::Grow.as_str(),
            Self::Churn { .. } => Name::Churn.as_str(),
            Self::Hold { .. } => Name::Hold.as_str(),
            Self::Release { .. } => Name::Release.as_str(),
            Self::Cache { .. } => Name::Cache.as_str(),
        }
    }

    #[must_use]
    pub const fn target_bytes(&self) -> Option<NonZeroU64> {
        match *self {
            Self::Grow { target_bytes, .. }
            | Self::Release { target_bytes, .. }
            | Self::Cache { target_bytes, .. } => Some(target_bytes),
            Self::Churn { .. } | Self::Hold { .. } => None,
        }
    }

    #[must_use]
    pub const fn target_time(&self) -> Option<Duration> {
        match *self {
            Self::Churn { target, .. } | Self::Hold { target } => Some(target),
            Self::Grow { .. } | Self::Release { .. } | Self::Cache { .. } => None,
        }
    }

    #[must_use]
    const fn advance(&self, frontier: u64) -> u64 {
        match *self {
            Self::Grow { target_bytes, .. } => frontier.saturating_add(target_bytes.get()),
            Self::Release { target_bytes, .. } => frontier - target_bytes.get(),
            Self::Churn { .. } | Self::Hold { .. } | Self::Cache { .. } => frontier,
        }
    }
}

impl FromStr for Kind {
    type Err = PlanError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Spec { text }.kind()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Plan {
    phases: Vec<Phase>,

    #[serde(skip)]
    peak: u64,
}

impl Plan {
    /// Reads the specs in order.
    pub fn parse<I, S>(specs: I) -> Result<Self, PlanError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut phases = Vec::new();
        let mut frontier = 0_u64;
        let mut peak = 0_u64;

        for (index, spec) in specs.into_iter().enumerate() {
            let spec = spec.as_ref();
            let kind = spec.parse::<Kind>()?;

            match kind {
                Kind::Release { target_bytes, .. } if target_bytes.get() > frontier => {
                    return Err(PlanError::Underflow {
                        index,
                        spec: spec.to_owned(),
                        over: target_bytes.get() - frontier,
                        frontier,
                    });
                }
                Kind::Churn { .. } if frontier == 0 => {
                    return Err(PlanError::Empty {
                        index,
                        spec: spec.to_owned(),
                    });
                }
                _ => {}
            }

            frontier = kind.advance(frontier);
            peak = peak.max(frontier);

            phases.push(Phase {
                spec: spec.to_owned(),
                kind,
            });
        }

        Ok(Self { phases, peak })
    }

    #[must_use]
    pub fn phases(&self) -> &[Phase] {
        &self.phases
    }

    #[must_use]
    pub const fn peak(&self) -> u64 {
        self.peak
    }

    /// Where cache phases write: `cache_dir`, if the plan has any.
    pub fn cache_dir(&self, cache_dir: Option<PathBuf>) -> Result<Option<PathBuf>, PlanError> {
        self.phases
            .iter()
            .enumerate()
            .find(|(_, phase)| matches!(phase.kind, Kind::Cache { .. }))
            .map(|(index, phase)| {
                cache_dir.ok_or_else(|| PlanError::Scratchless {
                    index,
                    spec: phase.spec.clone(),
                })
            })
            .transpose()
    }

    pub fn arena(&self, requested: Option<u64>) -> Result<NonZeroU64, PlanError> {
        let Some(requested) = requested else {
            return NonZeroU64::new(self.peak).ok_or(PlanError::Groundless);
        };

        let arena = NonZeroU64::new(requested).ok_or(PlanError::ZeroArena)?;

        let mut frontier = 0_u64;

        for (index, phase) in self.phases.iter().enumerate() {
            frontier = phase.kind.advance(frontier);

            if frontier > arena.get() {
                return Err(PlanError::Overflow {
                    index,
                    spec: phase.spec.clone(),
                    frontier,
                    arena: arena.get(),
                });
            }
        }

        Ok(arena)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PlanError {
    #[error("{spec:?} is not a phase: expected grow=, churn=, hold=, release=, or cache=")]
    Kind { spec: String },

    #[error("{spec:?} carries {part:?}, which is not key=value")]
    Pair { spec: String, part: String },

    #[error("{spec:?} carries {key:?}, which {kind} does not take")]
    Key {
        spec: String,
        key: String,
        kind: &'static str,
    },

    #[error("{spec:?} carries {key} twice")]
    Repeated { spec: String, key: &'static str },

    #[error("{spec:?} carries {value:?} for {key}, which is not a byte count")]
    Size {
        spec: String,
        key: &'static str,
        value: String,
    },

    #[error("{spec:?} carries {value:?} for {key}, which is not a duration")]
    Time {
        spec: String,
        key: &'static str,
        value: String,
    },

    #[error("{spec:?} carries {value:?} for rate, which is not a byte count per second")]
    Rate { spec: String, value: String },

    #[error("{spec:?} carries a {key} of zero")]
    Zero { spec: String, key: &'static str },

    #[error("{spec:?} names no rate, which churn needs")]
    Rateless { spec: String },

    #[error(
        "phase {index} {spec:?} retreats {over} bytes past the start of the arena, with only {frontier} bytes grown"
    )]
    Underflow {
        index: usize,
        spec: String,
        frontier: u64,
        over: u64,
    },

    #[error("phase {index} {spec:?} churns an arena nothing has grown into yet")]
    Empty { index: usize, spec: String },

    #[error("phase {index} {spec:?} reaches {frontier} bytes, past the {arena} byte arena")]
    Overflow {
        index: usize,
        spec: String,
        frontier: u64,
        arena: u64,
    },

    #[error("phase {index} {spec:?} writes page cache, but no --cache-dir names where")]
    Scratchless { index: usize, spec: String },

    #[error("the plan never grows, so there is nothing to map: name an --arena")]
    Groundless,

    #[error("--arena is zero, which maps nothing")]
    ZeroArena,
}

/// Which phase a spec names, read before any of its options are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Name {
    Grow,
    Churn,
    Hold,
    Release,
    Cache,
}

impl Name {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "grow" => Some(Self::Grow),
            "churn" => Some(Self::Churn),
            "hold" => Some(Self::Hold),
            "release" => Some(Self::Release),
            "cache" => Some(Self::Cache),
            _ => None,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Grow => "grow",
            Self::Churn => "churn",
            Self::Hold => "hold",
            Self::Release => "release",
            Self::Cache => "cache",
        }
    }

    /// Whether the phase moves bytes, and so takes a rate and a chunk.
    const fn moves_bytes(self) -> bool {
        !matches!(self, Self::Hold)
    }
}

#[derive(Debug, Clone, Copy)]
struct Spec<'a> {
    text: &'a str,
}

impl Spec<'_> {
    fn kind(self) -> Result<Kind, PlanError> {
        let mut parts = self.text.split(',');

        let (name, target) = parts
            .next()
            .and_then(|head| head.split_once('='))
            .and_then(|(name, target)| Name::parse(name).map(|name| (name, target)))
            .ok_or_else(|| PlanError::Kind {
                spec: self.text.into(),
            })?;

        let mut rate = None;
        let mut chunk = None;

        for part in parts {
            let (key, value) = part.split_once('=').ok_or_else(|| PlanError::Pair {
                spec: self.text.into(),
                part: part.into(),
            })?;

            match key {
                "rate" if name.moves_bytes() => {
                    let parsed = self.rate(value)?;

                    self.set(&mut rate, parsed, "rate")?;
                }
                "chunk" if name.moves_bytes() => {
                    let parsed = self.size("chunk", value)?;

                    self.set(&mut chunk, parsed, "chunk")?;
                }
                _ => {
                    return Err(PlanError::Key {
                        spec: self.text.into(),
                        key: key.into(),
                        kind: name.as_str(),
                    });
                }
            }
        }

        match name {
            Name::Grow => Ok(Kind::Grow {
                target_bytes: self.size(name.as_str(), target)?,
                rate_bps: rate,
                chunk: chunk.unwrap_or(DEFAULT_CHUNK),
            }),
            Name::Churn => Ok(Kind::Churn {
                target: self.time(name.as_str(), target)?,
                rate_bps: rate.ok_or_else(|| PlanError::Rateless {
                    spec: self.text.into(),
                })?,
                chunk: chunk.unwrap_or(DEFAULT_CHUNK),
            }),
            Name::Hold => Ok(Kind::Hold {
                target: self.time(name.as_str(), target)?,
            }),
            Name::Release => Ok(Kind::Release {
                target_bytes: self.size(name.as_str(), target)?,
                rate_bps: rate,
                chunk: chunk.unwrap_or(DEFAULT_RELEASE_CHUNK),
            }),
            Name::Cache => Ok(Kind::Cache {
                target_bytes: self.size(name.as_str(), target)?,
                rate_bps: rate,
                chunk: chunk.unwrap_or(DEFAULT_CHUNK),
            }),
        }
    }

    fn set<T>(self, slot: &mut Option<T>, value: T, key: &'static str) -> Result<(), PlanError> {
        if slot.replace(value).is_some() {
            return Err(PlanError::Repeated {
                spec: self.text.into(),
                key,
            });
        }

        Ok(())
    }

    fn size(self, key: &'static str, value: &str) -> Result<NonZeroU64, PlanError> {
        let bytes = value
            .parse::<ByteSize>()
            .map_err(|_| PlanError::Size {
                spec: self.text.into(),
                key,
                value: value.into(),
            })?
            .as_u64();

        NonZeroU64::new(bytes).ok_or_else(|| PlanError::Zero {
            spec: self.text.into(),
            key,
        })
    }

    fn time(self, key: &'static str, value: &str) -> Result<Duration, PlanError> {
        let duration = humantime::parse_duration(value).map_err(|_| PlanError::Time {
            spec: self.text.into(),
            key,
            value: value.into(),
        })?;

        if duration.is_zero() {
            return Err(PlanError::Zero {
                spec: self.text.into(),
                key,
            });
        }

        Ok(duration)
    }

    fn rate(self, value: &str) -> Result<NonZeroU64, PlanError> {
        let per_second = value.strip_suffix("/s").ok_or_else(|| PlanError::Rate {
            spec: self.text.into(),
            value: value.into(),
        })?;

        self.size("rate", per_second).map_err(|error| match error {
            PlanError::Size { .. } => PlanError::Rate {
                spec: self.text.into(),
                value: value.into(),
            },
            other => other,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KIB: u64 = 1 << 10;
    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;

    fn bytes(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).expect("a positive byte count")
    }

    fn phase(spec: &str) -> Kind {
        spec.parse().expect("the spec is a workable phase")
    }

    fn rejects(specs: &[&str]) -> String {
        Plan::parse(specs)
            .expect_err("the specs are not a workable plan")
            .to_string()
    }

    fn plan(specs: &[&str]) -> Plan {
        Plan::parse(specs).expect("the specs are a workable plan")
    }

    #[test]
    fn a_spec_that_sets_everything_is_taken_at_its_word() {
        assert_eq!(
            phase("grow=2GiB,rate=100MiB/s,chunk=4KiB"),
            Kind::Grow {
                target_bytes: bytes(2 * GIB),
                rate_bps: Some(bytes(100 * MIB)),
                chunk: bytes(4 * KIB),
            }
        );
        assert_eq!(
            phase("churn=1m 30s,rate=500MiB/s,chunk=2MiB"),
            Kind::Churn {
                target: Duration::from_secs(90),
                rate_bps: bytes(500 * MIB),
                chunk: bytes(2 * MIB),
            }
        );
        assert_eq!(
            phase("hold=250ms"),
            Kind::Hold {
                target: Duration::from_millis(250),
            }
        );
        assert_eq!(
            phase("release=1GiB,rate=1GiB/s,chunk=8MiB"),
            Kind::Release {
                target_bytes: bytes(GIB),
                rate_bps: Some(bytes(GIB)),
                chunk: bytes(8 * MIB),
            }
        );
        assert_eq!(
            phase("cache=3GiB,rate=256MiB/s,chunk=16MiB"),
            Kind::Cache {
                target_bytes: bytes(3 * GIB),
                rate_bps: Some(bytes(256 * MIB)),
                chunk: bytes(16 * MIB),
            }
        );
    }

    #[test]
    fn a_cache_phase_leaves_the_frontier_where_it_was() {
        assert_eq!(
            plan(&["grow=1GiB", "cache=4GiB", "grow=1GiB"]).peak(),
            2 * GIB
        );
    }

    #[test]
    fn a_plan_that_writes_page_cache_needs_somewhere_to_write_it() {
        let plan = plan(&["grow=1GiB", "cache=4GiB"]);
        let dir = PathBuf::from("/var/cache/stress");

        assert_eq!(
            plan.cache_dir(None)
                .expect_err("the plan has nowhere to write")
                .to_string(),
            "phase 1 \"cache=4GiB\" writes page cache, but no --cache-dir names where"
        );
        assert_eq!(
            plan.cache_dir(Some(dir.clone()))
                .expect("the plan has somewhere to write"),
            Some(dir)
        );
    }

    #[test]
    fn a_plan_without_cache_phases_has_no_use_for_a_cache_dir() {
        let plan = plan(&["grow=1GiB"]);

        assert_eq!(plan.cache_dir(None).expect("nothing writes"), None);
        assert_eq!(
            plan.cache_dir(Some(PathBuf::from("/var/cache/stress")))
                .expect("nothing writes"),
            None
        );
    }

    #[test]
    fn a_spec_that_sets_nothing_optional_bursts_at_the_default_chunk() {
        assert_eq!(
            phase("grow=512MiB"),
            Kind::Grow {
                target_bytes: bytes(512 * MIB),
                rate_bps: None,
                chunk: bytes(MIB),
            }
        );
        assert_eq!(
            phase("release=512MiB"),
            Kind::Release {
                target_bytes: bytes(512 * MIB),
                rate_bps: None,
                chunk: bytes(64 * MIB),
            }
        );
    }

    #[test]
    fn the_order_of_the_options_is_the_writers_business() {
        assert_eq!(
            phase("grow=1GiB,chunk=2MiB,rate=64MiB/s"),
            phase("grow=1GiB,rate=64MiB/s,chunk=2MiB")
        );
    }

    #[test]
    fn a_plan_keeps_the_specs_in_the_order_it_was_given_them() {
        let plan = plan(&["grow=8MiB", "hold=1s", "release=8MiB"]);

        assert_eq!(
            plan.phases()
                .iter()
                .map(|phase| phase.kind.name())
                .collect::<Vec<_>>(),
            vec!["grow", "hold", "release"]
        );
        assert_eq!(plan.phases()[1].spec, "hold=1s");
    }

    #[test]
    fn a_phase_knows_which_target_it_carries() {
        assert_eq!(phase("grow=8MiB").target_bytes(), Some(bytes(8 * MIB)));
        assert_eq!(phase("grow=8MiB").target_time(), None);
        assert_eq!(phase("hold=1s").target_bytes(), None);
        assert_eq!(phase("hold=1s").target_time(), Some(Duration::from_secs(1)));
    }

    #[test]
    fn an_unknown_phase_names_the_spec() {
        assert_eq!(
            rejects(&["sprint=2GiB"]),
            "\"sprint=2GiB\" is not a phase: expected grow=, churn=, hold=, release=, or cache="
        );
        assert_eq!(
            rejects(&["grow"]),
            "\"grow\" is not a phase: expected grow=, churn=, hold=, release=, or cache="
        );
    }

    #[test]
    fn an_unknown_key_names_itself_and_the_phase_that_refused_it() {
        assert_eq!(
            rejects(&["grow=1GiB,speed=100MiB/s"]),
            "\"grow=1GiB,speed=100MiB/s\" carries \"speed\", which grow does not take"
        );
        assert_eq!(
            rejects(&["hold=30s,rate=1MiB/s"]),
            "\"hold=30s,rate=1MiB/s\" carries \"rate\", which hold does not take"
        );
        assert_eq!(
            rejects(&["hold=30s,chunk=1MiB"]),
            "\"hold=30s,chunk=1MiB\" carries \"chunk\", which hold does not take"
        );
    }

    #[test]
    fn an_option_that_is_not_a_pair_names_itself() {
        assert_eq!(
            rejects(&["grow=1GiB,burst"]),
            "\"grow=1GiB,burst\" carries \"burst\", which is not key=value"
        );
    }

    #[test]
    fn an_option_given_twice_names_itself() {
        assert_eq!(
            rejects(&["grow=1GiB,chunk=1MiB,chunk=2MiB"]),
            "\"grow=1GiB,chunk=1MiB,chunk=2MiB\" carries chunk twice"
        );
    }

    #[test]
    fn a_malformed_size_names_the_key_that_carries_it() {
        assert_eq!(
            rejects(&["grow=2 lorries"]),
            "\"grow=2 lorries\" carries \"2 lorries\" for grow, which is not a byte count"
        );
        assert_eq!(
            rejects(&["grow=1GiB,chunk=wat"]),
            "\"grow=1GiB,chunk=wat\" carries \"wat\" for chunk, which is not a byte count"
        );
    }

    #[test]
    fn a_malformed_duration_names_the_key_that_carries_it() {
        assert_eq!(
            rejects(&["hold=3 fortnights"]),
            "\"hold=3 fortnights\" carries \"3 fortnights\" for hold, which is not a duration"
        );
    }

    #[test]
    fn a_rate_without_a_second_is_not_a_rate() {
        assert_eq!(
            rejects(&["grow=1GiB,rate=100MiB"]),
            "\"grow=1GiB,rate=100MiB\" carries \"100MiB\" for rate, which is not a byte count per second"
        );
        assert_eq!(
            rejects(&["grow=1GiB,rate=quickly/s"]),
            "\"grow=1GiB,rate=quickly/s\" carries \"quickly/s\" for rate, which is not a byte count per second"
        );
    }

    #[test]
    fn a_zero_quantity_names_the_key_that_carries_it() {
        assert_eq!(rejects(&["grow=0B"]), "\"grow=0B\" carries a grow of zero");
        assert_eq!(
            rejects(&["grow=1GiB,chunk=0B"]),
            "\"grow=1GiB,chunk=0B\" carries a chunk of zero"
        );
        assert_eq!(
            rejects(&["grow=1GiB,rate=0B/s"]),
            "\"grow=1GiB,rate=0B/s\" carries a rate of zero"
        );
        assert_eq!(rejects(&["hold=0s"]), "\"hold=0s\" carries a hold of zero");
    }

    #[test]
    fn a_churn_without_a_rate_is_refused() {
        assert_eq!(
            rejects(&["grow=1GiB", "churn=30s"]),
            "\"churn=30s\" names no rate, which churn needs"
        );
    }

    #[test]
    fn a_churn_over_an_empty_arena_is_refused() {
        assert_eq!(
            rejects(&["churn=30s,rate=1MiB/s"]),
            "phase 0 \"churn=30s,rate=1MiB/s\" churns an arena nothing has grown into yet"
        );
        assert_eq!(
            rejects(&["grow=1GiB", "release=1GiB", "churn=30s,rate=1MiB/s"]),
            "phase 2 \"churn=30s,rate=1MiB/s\" churns an arena nothing has grown into yet"
        );
    }

    #[test]
    fn a_plan_that_releases_more_than_it_grew_is_refused() {
        assert_eq!(
            rejects(&["release=1GiB"]),
            "phase 0 \"release=1GiB\" retreats 1073741824 bytes past the start of the arena, with only 0 bytes grown"
        );
        assert_eq!(
            rejects(&["grow=1GiB", "release=512MiB", "release=1GiB"]),
            "phase 2 \"release=1GiB\" retreats 536870912 bytes past the start of the arena, with only 536870912 bytes grown"
        );
    }

    #[test]
    fn the_peak_is_the_highest_the_frontier_ever_reaches() {
        assert_eq!(plan(&["grow=2GiB"]).peak(), 2 * GIB);
        assert_eq!(plan(&["grow=2GiB", "release=1GiB"]).peak(), 2 * GIB);
        assert_eq!(
            plan(&["grow=2GiB", "release=1GiB", "grow=512MiB"]).peak(),
            2 * GIB
        );
        assert_eq!(
            plan(&["grow=2GiB", "release=1GiB", "grow=2GiB"]).peak(),
            3 * GIB
        );
        assert_eq!(
            plan(&[
                "grow=1GiB",
                "churn=30s,rate=1MiB/s",
                "hold=30s",
                "release=512MiB",
                "grow=1GiB",
            ])
            .peak(),
            3 * GIB / 2
        );
    }

    #[test]
    fn a_plan_without_an_arena_maps_its_own_peak() {
        assert_eq!(
            plan(&["grow=2GiB", "release=1GiB", "grow=512MiB"]).arena(None),
            Ok(bytes(2 * GIB))
        );
    }

    #[test]
    fn a_plan_that_only_waits_has_nothing_to_map() {
        assert_eq!(plan(&["hold=30s"]).arena(None), Err(PlanError::Groundless));
        assert_eq!(
            plan(&["hold=30s"]).arena(Some(GIB)),
            Ok(bytes(GIB)),
            "an arena given outright is still a mapping"
        );
    }

    #[test]
    fn an_arena_the_plan_fits_inside_is_taken_at_its_word() {
        assert_eq!(
            plan(&["grow=2GiB"]).arena(Some(8 * GIB)),
            Ok(bytes(8 * GIB))
        );
        assert_eq!(
            plan(&["grow=2GiB"]).arena(Some(2 * GIB)),
            Ok(bytes(2 * GIB))
        );
    }

    #[test]
    fn a_plan_that_grows_past_its_arena_names_the_phase_that_does_it() {
        let plan = plan(&["grow=2GiB", "release=1GiB", "grow=2GiB"]);

        assert_eq!(
            plan.arena(Some(2 * GIB))
                .expect_err("the regrow runs off the end")
                .to_string(),
            "phase 2 \"grow=2GiB\" reaches 3221225472 bytes, past the 2147483648 byte arena"
        );
    }

    #[test]
    fn an_arena_of_nothing_is_refused() {
        assert_eq!(
            plan(&["grow=2GiB"]).arena(Some(0)),
            Err(PlanError::ZeroArena)
        );
    }

    #[test]
    fn a_plan_serialises_as_the_specs_it_was_given() {
        let plan = plan(&["grow=8MiB,rate=32MiB/s", "hold=200ms", "release=8MiB"]);

        assert_eq!(
            serde_json::to_string(&plan).expect("a plan serialises"),
            concat!(
                r#"[{"spec":"grow=8MiB,rate=32MiB/s","kind":"grow","target_bytes":8388608,"#,
                r#""rate_bps":33554432,"chunk":1048576},"#,
                r#"{"spec":"hold=200ms","kind":"hold","target_ms":200},"#,
                r#"{"spec":"release=8MiB","kind":"release","target_bytes":8388608,"#,
                r#""chunk":67108864}]"#,
            )
        );
    }
}
