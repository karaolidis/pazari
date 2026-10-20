use std::{
    io::Write,
    num::NonZeroU64,
    path::PathBuf,
    process,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use pazari_types::Interval;
use thiserror::Error;
use tracing::{info, warn};

use crate::{
    pace::Pacer,
    plan::{Kind, Plan},
    region::{Fill, Region, RegionError},
    report::{ReportError, Reporter, Started},
    scratch::{Scratch, ScratchError},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub plan: Plan,
    pub arena: NonZeroU64,
    pub interval: Interval,
    pub no_reserve: bool,
    pub no_thp: bool,
    pub fill: Fill,
    pub cache_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Every phase ran to its target.
    Done,

    /// A signal arrived and the run stopped between chunks.
    Interrupted,
}

pub fn run<W: Write>(
    options: &Options,
    out: &mut Reporter<W>,
    interrupt: &AtomicBool,
) -> Result<Outcome, RunError> {
    let region = Region::map(
        options.arena,
        options.no_reserve,
        options.no_thp,
        options.fill,
    )?;

    info!(
        arena = region.bytes(),
        page_size = region.page(),
        phases = options.plan.phases().len(),
        "workload started"
    );

    out.started(Started {
        pid: process::id(),
        page_size: region.page(),
        arena: region.bytes(),
        no_reserve: options.no_reserve,
        no_thp: options.no_thp,
        fill: options.fill,
        cache_dir: options.cache_dir.as_deref(),
        plan: &options.plan,
    })?;

    let scratch = options
        .cache_dir
        .as_deref()
        .map(Scratch::create)
        .transpose()?;

    let mut walk = Walk {
        region,
        scratch,
        out,
        interrupt,
        interval: options.interval.get(),
        frontier: 0,
        index: 0,
        started: Instant::now(),
        touched: 0,
        due: Duration::ZERO,
        marked: Duration::ZERO,
        counted: 0,
    };

    for (index, phase) in options.plan.phases().iter().enumerate() {
        walk.index = index;
        walk.out.phase(index, &phase.kind)?;

        if walk.phase(&phase.kind)? == Flow::Stop {
            warn!(
                index,
                spec = phase.spec.as_str(),
                "workload stopped by signal"
            );
            walk.out.interrupted(index)?;

            return Ok(Outcome::Interrupted);
        }

        walk.out.completed(index)?;
    }

    info!("workload finished");
    walk.out.done()?;

    Ok(Outcome::Done)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Go,
    Stop,
}

struct Walk<'a, W> {
    region: Region,
    scratch: Option<Scratch>,
    out: &'a mut Reporter<W>,
    interrupt: &'a AtomicBool,
    interval: Duration,
    frontier: u64,
    index: usize,
    started: Instant,
    touched: u64,
    due: Duration,
    marked: Duration,
    counted: u64,
}

impl<W: Write> Walk<'_, W> {
    fn phase(&mut self, kind: &Kind) -> Result<Flow, RunError> {
        self.started = Instant::now();
        self.touched = 0;
        self.due = self.interval;
        self.marked = Duration::ZERO;
        self.counted = 0;

        match *kind {
            Kind::Grow {
                target_bytes,
                rate_bps,
                chunk,
            } => self.grow(target_bytes.get(), Pacer::new(rate_bps, chunk)),

            Kind::Churn {
                target,
                rate_bps,
                chunk,
            } => self.churn(target, Pacer::at(rate_bps, chunk)),

            Kind::Hold { target } => self.wait(target),

            Kind::Release {
                target_bytes,
                rate_bps,
                chunk,
            } => self.retreat(target_bytes.get(), Pacer::new(rate_bps, chunk)),

            Kind::Cache {
                target_bytes,
                rate_bps,
                chunk,
            } => self.cache(target_bytes.get(), Pacer::new(rate_bps, chunk)),
        }
    }

    fn grow(&mut self, target: u64, pacer: Pacer) -> Result<Flow, RunError> {
        while self.touched < target {
            if self.wait(pacer.deadline(self.touched))? == Flow::Stop {
                return Ok(Flow::Stop);
            }

            let step = pacer.step(target.saturating_sub(self.touched));
            let frontier = self.frontier;

            self.region.touch(frontier, frontier.saturating_add(step))?;

            self.frontier = frontier.saturating_add(step);
            self.touched = self.touched.saturating_add(step);
        }

        Ok(Flow::Go)
    }

    fn churn(&mut self, target: Duration, pacer: Pacer) -> Result<Flow, RunError> {
        if self.frontier == 0 {
            return self.wait(target);
        }

        let mut cursor = 0_u64;

        loop {
            if self.wait(pacer.deadline(self.touched).min(target))? == Flow::Stop {
                return Ok(Flow::Stop);
            }

            if self.started.elapsed() >= target {
                return Ok(Flow::Go);
            }

            let step = pacer.step(self.frontier.saturating_sub(cursor));

            self.region.touch(cursor, cursor.saturating_add(step))?;

            cursor = cursor.saturating_add(step);

            if cursor >= self.frontier {
                cursor = 0;
            }

            self.touched = self.touched.saturating_add(step);
        }
    }

    fn retreat(&mut self, target: u64, pacer: Pacer) -> Result<Flow, RunError> {
        while self.touched < target {
            if self.wait(pacer.deadline(self.touched))? == Flow::Stop {
                return Ok(Flow::Stop);
            }

            let step = pacer.step(target.saturating_sub(self.touched));
            let frontier = self.frontier;

            self.region
                .release(frontier.saturating_sub(step), frontier)?;

            self.frontier = frontier.saturating_sub(step);
            self.touched = self.touched.saturating_add(step);
        }

        Ok(Flow::Go)
    }

    fn cache(&mut self, target: u64, pacer: Pacer) -> Result<Flow, RunError> {
        while self.touched < target {
            if self.wait(pacer.deadline(self.touched))? == Flow::Stop {
                return Ok(Flow::Stop);
            }

            let step = pacer.step(target.saturating_sub(self.touched));

            self.scratch
                .as_mut()
                .ok_or(ScratchError::Missing { index: self.index })?
                .fill(step)?;

            self.touched = self.touched.saturating_add(step);
        }

        Ok(Flow::Go)
    }

    fn wait(&mut self, deadline: Duration) -> Result<Flow, RunError> {
        loop {
            if self.interrupt.load(Ordering::Relaxed) {
                return Ok(Flow::Stop);
            }

            let elapsed = self.started.elapsed();

            if elapsed >= self.due {
                self.report(elapsed)?;
                continue;
            }

            if elapsed >= deadline {
                return Ok(Flow::Go);
            }

            thread::sleep(deadline.min(self.due).saturating_sub(elapsed));
        }
    }

    fn report(&mut self, elapsed: Duration) -> Result<(), RunError> {
        let rate = Pacer::measure(
            self.touched.saturating_sub(self.counted),
            elapsed.saturating_sub(self.marked),
        );

        self.out
            .progress(self.index, self.touched, self.frontier, rate)?;

        self.marked = elapsed;
        self.counted = self.touched;
        self.due = due(self.interval, elapsed);

        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum RunError {
    #[error(transparent)]
    Region(#[from] RegionError),

    #[error(transparent)]
    Report(#[from] ReportError),

    #[error(transparent)]
    Scratch(#[from] ScratchError),
}

fn due(interval: Duration, elapsed: Duration) -> Duration {
    let period = interval.as_nanos().max(1);
    let nanos = (elapsed.as_nanos() / period + 1).saturating_mul(period);

    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::Value;

    use super::*;

    const MIB: u64 = 1 << 20;

    fn options(specs: &[&str], interval: u64) -> Options {
        let plan = Plan::parse(specs).expect("the specs are a workable plan");

        Options {
            arena: plan.arena(None).expect("the plan grows into something"),
            interval: Interval::try_new(Duration::from_millis(interval))
                .expect("a positive interval under a day"),
            no_reserve: false,
            no_thp: false,
            fill: Fill::Index,
            cache_dir: None,
            plan,
        }
    }

    fn walk(options: &Options, interrupt: &AtomicBool) -> (Outcome, Vec<Value>) {
        let mut reported = Vec::new();

        let outcome = {
            let mut out = Reporter::new(&mut reported);

            run(options, &mut out, interrupt).expect("the workload runs")
        };

        let events = String::from_utf8(reported)
            .expect("the stream is text")
            .lines()
            .map(|line| serde_json::from_str(line).expect("every line is one JSON object"))
            .collect();

        (outcome, events)
    }

    fn kinds(events: &[Value]) -> Vec<&str> {
        events
            .iter()
            .map(|event| event["type"].as_str().expect("every event is tagged"))
            .collect()
    }

    fn of_kind<'a>(events: &'a [Value], kind: &str) -> Vec<&'a Value> {
        events
            .iter()
            .filter(|event| event["type"] == kind)
            .collect()
    }

    fn field(event: &Value, name: &str) -> u64 {
        event[name]
            .as_u64()
            .unwrap_or_else(|| panic!("{name} is a count: {event}"))
    }

    #[test]
    fn a_reporting_instant_is_always_the_next_one_after_now() {
        let interval = Duration::from_millis(100);

        assert_eq!(due(interval, Duration::ZERO), Duration::from_millis(100));
        assert_eq!(
            due(interval, Duration::from_millis(99)),
            Duration::from_millis(100)
        );
        assert_eq!(
            due(interval, Duration::from_millis(100)),
            Duration::from_millis(200)
        );
        assert_eq!(
            due(interval, Duration::from_millis(550)),
            Duration::from_millis(600),
            "a slow chunk does not owe a burst of reports"
        );
    }

    #[test]
    fn a_plan_opens_with_what_it_was_asked_for() {
        let options = options(&["grow=2MiB"], 10);
        let (_, events) = walk(&options, &AtomicBool::new(false));

        let started = events.first().expect("the run said something");

        assert_eq!(started["type"], "started");
        assert_eq!(field(started, "pid"), u64::from(process::id()));
        assert_eq!(field(started, "arena"), 2 * MIB);
        assert_eq!(started["no_reserve"], false);
        assert_eq!(started["no_thp"], false);
        assert_eq!(started["fill"], "index");
        assert_eq!(started["plan"][0]["spec"], "grow=2MiB");
        assert!(field(started, "page_size") >= 4096);
    }

    #[test]
    fn a_cache_phase_runs_and_cleans_up_after_itself() {
        let dir = tempfile::TempDir::new().expect("a temporary directory");
        let options = Options {
            cache_dir: Some(dir.path().to_owned()),
            ..options(&["grow=1MiB", "cache=3MiB,chunk=1MiB"], 10)
        };

        let (outcome, events) = walk(&options, &AtomicBool::new(false));

        assert_eq!(outcome, Outcome::Done);
        assert_eq!(
            of_kind(&events, "phase")
                .iter()
                .map(|event| event["kind"].as_str().expect("a named kind"))
                .collect::<Vec<_>>(),
            vec!["grow", "cache"]
        );
        assert_eq!(
            fs::read_dir(dir.path())
                .expect("a readable directory")
                .count(),
            0,
            "the scratch file outlived the run"
        );
    }

    #[test]
    fn a_cache_phase_without_a_cache_dir_fails_loudly() {
        let mut reported = Vec::new();
        let mut out = Reporter::new(&mut reported);

        let error = run(
            &options(&["grow=1MiB", "cache=1MiB"], 10),
            &mut out,
            &AtomicBool::new(false),
        )
        .expect_err("there is nowhere to write");

        assert_eq!(
            error.to_string(),
            "phase 1 writes page cache, but the run has no cache directory"
        );
    }

    #[test]
    fn a_plan_runs_its_phases_in_order_and_ends_saying_so() {
        let options = options(&["grow=2MiB", "hold=30ms", "release=2MiB"], 10);
        let (outcome, events) = walk(&options, &AtomicBool::new(false));

        assert_eq!(outcome, Outcome::Done);

        let kinds = kinds(&events);

        assert_eq!(kinds.first(), Some(&"started"));
        assert_eq!(kinds.last(), Some(&"done"));
        assert_eq!(
            kinds
                .iter()
                .filter(|kind| **kind == "phase" || **kind == "completed")
                .copied()
                .collect::<Vec<_>>(),
            vec![
                "phase",
                "completed",
                "phase",
                "completed",
                "phase",
                "completed"
            ]
        );

        assert_eq!(
            of_kind(&events, "phase")
                .iter()
                .map(|event| event["kind"].as_str().expect("a named kind"))
                .collect::<Vec<_>>(),
            vec!["grow", "hold", "release"]
        );
        assert_eq!(
            of_kind(&events, "completed")
                .iter()
                .map(|event| field(event, "index"))
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn the_clock_only_ever_moves_forward() {
        let options = options(&["grow=2MiB", "hold=30ms", "release=2MiB"], 10);
        let (_, events) = walk(&options, &AtomicBool::new(false));

        let stamps: Vec<u64> = events
            .iter()
            .filter_map(|event| event["elapsed_ms"].as_u64())
            .collect();

        assert!(stamps.len() > 1, "nothing was stamped");
        assert!(
            stamps.windows(2).all(|pair| pair[0] <= pair[1]),
            "the clock went backwards: {stamps:?}"
        );
    }

    #[test]
    fn the_frontier_follows_the_plan_up_and_back_down() {
        let options = options(
            &[
                "grow=4MiB,rate=64MiB/s",
                "release=4MiB,rate=64MiB/s,chunk=1MiB",
            ],
            20,
        );
        let (outcome, events) = walk(&options, &AtomicBool::new(false));

        assert_eq!(outcome, Outcome::Done);

        let frontiers: Vec<u64> = of_kind(&events, "progress")
            .iter()
            .map(|event| field(event, "frontier"))
            .collect();

        assert!(frontiers.len() > 2, "nothing reported: {frontiers:?}");
        assert!(
            frontiers.iter().all(|frontier| *frontier <= 4 * MIB),
            "the frontier ran past the arena: {frontiers:?}"
        );

        let peak = frontiers.iter().max().copied().unwrap_or_default();

        assert!(peak > 0, "the frontier never moved");
        assert!(
            frontiers
                .iter()
                .rev()
                .take(1)
                .all(|frontier| *frontier < peak),
            "the frontier never came back: {frontiers:?}"
        );
    }

    #[test]
    fn a_paced_phase_reports_what_it_is_moving() {
        let options = options(&["grow=4MiB,rate=32MiB/s"], 20);
        let (_, events) = walk(&options, &AtomicBool::new(false));

        let progress = of_kind(&events, "progress");

        assert!(
            progress.len() >= 2,
            "a paced phase reported {} times",
            progress.len()
        );
        assert!(
            progress
                .windows(2)
                .all(|pair| field(pair[0], "touched") <= field(pair[1], "touched")),
            "the bytes moved went backwards"
        );
        assert!(
            progress.iter().any(|event| field(event, "rate_bps") > 0),
            "a paced phase reported no rate at all"
        );
    }

    #[test]
    fn a_churn_rewrites_the_resident_set_without_moving_the_frontier() {
        let options = options(&["grow=2MiB", "churn=80ms,rate=32MiB/s,chunk=256KiB"], 20);
        let (outcome, events) = walk(&options, &AtomicBool::new(false));

        assert_eq!(outcome, Outcome::Done);

        let churned: Vec<&Value> = of_kind(&events, "progress")
            .into_iter()
            .filter(|event| field(event, "index") == 1)
            .collect();

        assert!(!churned.is_empty(), "the churn reported nothing");
        assert!(
            churned
                .iter()
                .all(|event| field(event, "frontier") == 2 * MIB),
            "the churn moved the frontier"
        );
        assert!(
            churned.iter().any(|event| field(event, "touched") > 0),
            "the churn wrote nothing"
        );
    }

    #[test]
    fn a_signal_stops_the_run_between_chunks_and_still_exits_cleanly() {
        let options = options(&["grow=1GiB,rate=1MiB/s"], 10);
        let (outcome, events) = walk(&options, &AtomicBool::new(true));

        assert_eq!(outcome, Outcome::Interrupted);
        assert_eq!(kinds(&events), vec!["started", "phase", "interrupted"]);
        assert_eq!(
            field(events.last().expect("an ending"), "index"),
            0,
            "the interruption named the wrong phase"
        );
    }

    #[test]
    fn a_plan_that_only_waits_still_holds_its_arena() {
        let mut options = options(&["grow=1MiB"], 10);

        options.plan = Plan::parse(["hold=30ms"]).expect("a workable plan");

        let (outcome, events) = walk(&options, &AtomicBool::new(false));

        assert_eq!(outcome, Outcome::Done);
        assert!(
            of_kind(&events, "progress")
                .iter()
                .all(|event| field(event, "touched") == 0),
            "an idle phase touched something"
        );
    }
}
