use pazari_proto::GuestMessage;

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

    StartReclaim { bytes: u64 },

    CancelReclaim,

    Send(GuestMessage),
}
