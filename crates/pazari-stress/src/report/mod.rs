use std::{
    io::{self, Write},
    num::NonZeroU64,
    path::Path,
    time::Duration,
};

use pazari_types::{Clock, Timestamp};
use serde::{Serialize, Serializer};
use serde_with::{DurationMilliSeconds, serde_as};
use thiserror::Error;

use crate::{
    plan::{Kind, Plan},
    region::{Commit, Fill, HugePages},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Started<'a> {
    pub pid: u32,
    pub page_size: u64,
    pub arena: u64,

    #[serde(rename = "no_reserve", serialize_with = "no_reserve")]
    pub commit: Commit,

    #[serde(rename = "no_thp", serialize_with = "no_thp")]
    pub huge_pages: HugePages,

    pub fill: Fill,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_dir: Option<&'a Path>,

    pub plan: &'a Plan,
}

#[serde_as]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event<'a> {
    Started(Started<'a>),

    Phase {
        index: usize,
        kind: &'static str,

        #[serde(skip_serializing_if = "Option::is_none")]
        target_bytes: Option<u64>,

        #[serde_as(as = "Option<DurationMilliSeconds<u64>>")]
        #[serde(rename = "target_ms", skip_serializing_if = "Option::is_none")]
        target: Option<Duration>,
    },

    Progress {
        index: usize,

        #[serde_as(as = "DurationMilliSeconds<u64>")]
        #[serde(rename = "elapsed_ms")]
        elapsed: Duration,

        touched: u64,
        frontier: u64,
        rate_bps: u64,
    },

    Completed {
        index: usize,

        #[serde_as(as = "DurationMilliSeconds<u64>")]
        #[serde(rename = "elapsed_ms")]
        elapsed: Duration,
    },

    Done {
        #[serde_as(as = "DurationMilliSeconds<u64>")]
        #[serde(rename = "elapsed_ms")]
        elapsed: Duration,
    },

    Interrupted {
        index: usize,

        #[serde_as(as = "DurationMilliSeconds<u64>")]
        #[serde(rename = "elapsed_ms")]
        elapsed: Duration,
    },

    Failed {
        #[serde_as(as = "DurationMilliSeconds<u64>")]
        #[serde(rename = "elapsed_ms")]
        elapsed: Duration,

        error: &'a str,
    },
}

impl Event<'_> {
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match *self {
            Self::Started(_) => "started",
            Self::Phase { .. } => "phase",
            Self::Progress { .. } => "progress",
            Self::Completed { .. } => "completed",
            Self::Done { .. } => "done",
            Self::Interrupted { .. } => "interrupted",
            Self::Failed { .. } => "failed",
        }
    }
}

#[derive(Debug)]
pub struct Reporter<W> {
    out: W,
    clock: Clock,
}

impl<W: Write> Reporter<W> {
    #[must_use]
    pub fn new(out: W) -> Self {
        Self {
            out,
            clock: Clock::start(),
        }
    }

    pub fn started(&mut self, started: Started<'_>) -> Result<(), ReportError> {
        self.emit(&Event::Started(started))
    }

    pub fn phase(&mut self, index: usize, kind: &Kind) -> Result<(), ReportError> {
        self.emit(&Event::Phase {
            index,
            kind: kind.name(),
            target_bytes: kind.target_bytes().map(NonZeroU64::get),
            target: kind.target_time(),
        })
    }

    pub fn progress(
        &mut self,
        index: usize,
        touched: u64,
        frontier: u64,
        rate_bps: u64,
    ) -> Result<(), ReportError> {
        self.emit(&Event::Progress {
            index,
            elapsed: self.elapsed(),
            touched,
            frontier,
            rate_bps,
        })
    }

    pub fn completed(&mut self, index: usize) -> Result<(), ReportError> {
        self.emit(&Event::Completed {
            index,
            elapsed: self.elapsed(),
        })
    }

    pub fn done(&mut self) -> Result<(), ReportError> {
        self.emit(&Event::Done {
            elapsed: self.elapsed(),
        })
    }

    pub fn interrupted(&mut self, index: usize) -> Result<(), ReportError> {
        self.emit(&Event::Interrupted {
            index,
            elapsed: self.elapsed(),
        })
    }

    pub fn failed(&mut self, error: &str) -> Result<(), ReportError> {
        self.emit(&Event::Failed {
            elapsed: self.elapsed(),
            error,
        })
    }

    /// The time since the reporter started.
    fn elapsed(&self) -> Duration {
        self.clock.now().saturating_duration_since(Timestamp::START)
    }

    fn emit(&mut self, event: &Event<'_>) -> Result<(), ReportError> {
        let kind = event.name();

        let line = serde_json::to_string(event).map_err(|source| ReportError::Encode {
            kind,
            source: Box::new(source),
        })?;

        writeln!(self.out, "{line}").map_err(|source| ReportError::Write { kind, source })?;

        self.out
            .flush()
            .map_err(|source| ReportError::Write { kind, source })
    }
}

#[derive(Debug, Error)]
pub enum ReportError {
    #[error("failed to serialise a {kind} event: {source}")]
    Encode {
        kind: &'static str,
        source: Box<serde_json::Error>,
    },

    #[error("failed to write a {kind} event: {source}")]
    Write {
        kind: &'static str,
        source: io::Error,
    },
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde hands fields by reference"
)]
fn no_reserve<S: Serializer>(commit: &Commit, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_bool(*commit == Commit::NoReserve)
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde hands fields by reference"
)]
fn no_thp<S: Serializer>(huge_pages: &HugePages, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_bool(*huge_pages == HugePages::Excluded)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    fn lines(reported: &[u8]) -> Vec<String> {
        String::from_utf8(reported.to_vec())
            .expect("the stream is text")
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn plan(specs: &[&str]) -> Plan {
        Plan::parse(specs, None).expect("the specs are a workable plan")
    }

    #[test]
    fn every_event_is_one_object_on_one_line() {
        let mut out = Reporter::new(Vec::new());
        let plan = plan(&["grow=8MiB"]);

        out.started(Started {
            pid: 42,
            page_size: 4096,
            arena: 8 * MIB,
            commit: Commit::Reserve,
            huge_pages: HugePages::Excluded,
            fill: Fill::Random,
            cache_dir: None,
            plan: &plan,
        })
        .expect("a writable stream");

        out.phase(0, &plan.phases()[0].kind)
            .expect("a writable stream");
        out.progress(0, MIB, MIB, 32 * MIB)
            .expect("a writable stream");
        out.completed(0).expect("a writable stream");
        out.done().expect("a writable stream");

        let lines = lines(&out.out);

        assert_eq!(lines.len(), 5);
        assert_eq!(
            lines[0],
            concat!(
                r#"{"type":"started","pid":42,"page_size":4096,"arena":8388608,"#,
                r#""no_reserve":false,"no_thp":true,"fill":"random","#,
                r#""plan":[{"spec":"grow=8MiB","kind":"grow","target_bytes":8388608,"chunk":1048576}]}"#,
            )
        );
        assert_eq!(
            lines[1],
            r#"{"type":"phase","index":0,"kind":"grow","target_bytes":8388608}"#
        );
        assert!(
            lines[2].starts_with(r#"{"type":"progress","index":0,"elapsed_ms":"#),
            "unexpected progress: {}",
            lines[2]
        );
        assert!(
            lines[2].ends_with(r#""touched":1048576,"frontier":1048576,"rate_bps":33554432}"#),
            "unexpected progress: {}",
            lines[2]
        );
        assert!(
            lines[3].starts_with(r#"{"type":"completed","index":0,"elapsed_ms":"#),
            "unexpected completion: {}",
            lines[3]
        );
        assert!(
            lines[4].starts_with(r#"{"type":"done","elapsed_ms":"#),
            "unexpected end: {}",
            lines[4]
        );
    }

    #[test]
    fn a_phase_carries_the_one_target_its_kind_has() {
        let mut out = Reporter::new(Vec::new());
        let plan = plan(&["grow=8MiB", "hold=200ms"]);

        out.phase(0, &plan.phases()[0].kind)
            .expect("a writable stream");
        out.phase(1, &plan.phases()[1].kind)
            .expect("a writable stream");

        assert_eq!(
            lines(&out.out),
            vec![
                r#"{"type":"phase","index":0,"kind":"grow","target_bytes":8388608}"#,
                r#"{"type":"phase","index":1,"kind":"hold","target_ms":200}"#,
            ]
        );
    }

    #[test]
    fn an_ending_that_is_not_the_planned_one_says_so() {
        let mut out = Reporter::new(Vec::new());

        out.interrupted(2).expect("a writable stream");
        out.failed("the arena did not map")
            .expect("a writable stream");

        let lines = lines(&out.out);

        assert!(
            lines[0].starts_with(r#"{"type":"interrupted","index":2,"elapsed_ms":"#),
            "unexpected interruption: {}",
            lines[0]
        );
        assert!(
            lines[1].ends_with(r#""error":"the arena did not map"}"#),
            "unexpected failure: {}",
            lines[1]
        );
    }

    #[test]
    fn a_stream_that_will_not_take_a_line_names_the_event() {
        struct Broken;

        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::BrokenPipe))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let error = Reporter::new(Broken)
            .done()
            .expect_err("the stream is shut");

        assert_eq!(
            error.to_string(),
            "failed to write a done event: broken pipe"
        );
    }
}
