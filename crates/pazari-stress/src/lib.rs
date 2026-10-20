//! pazari workload generator

pub mod pace;
pub mod plan;
pub mod region;
pub mod report;
pub mod run;
pub mod scratch;

pub use crate::{
    pace::Pacer,
    plan::{Kind, Phase, Plan, PlanError},
    region::{Commit, Fill, FillError, HugePages, Region, RegionError},
    report::{Event, ReportError, Reporter, Started},
    run::{Options, Outcome, RunError},
    scratch::{Scratch, ScratchError},
};
