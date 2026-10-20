use std::num::NonZeroU64;

use pazari_proto::GuestMessage;

/// The limits the workload starts under, before the engine handles any event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ground {
    pub max: u64,
    pub high: u64,
    pub oom_group: OomGroup,
}

/// Whether the kernel's OOM killer takes the workload as a whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OomGroup {
    /// One OOM kill takes every process in the cgroup.
    Whole,

    /// The OOM killer picks single processes.
    PerProcess,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    SetHigh(u64),
    SetMax(u64),
    SetOomGroup(OomGroup),

    StartReclaim { bytes: NonZeroU64 },

    CancelReclaim,

    Send(GuestMessage),
}
