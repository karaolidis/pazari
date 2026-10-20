use std::fmt;

/// Why the guest turned down the host's reclaim request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// A grant request is outstanding.
    Growing,

    /// The workload is reclaiming to make room.
    Reclaiming,

    /// A release is already in progress.
    Releasing,

    /// The workload is stalled at its ceiling.
    Exhausted,

    /// A reclaim has started and not yet reported.
    ReclaimPending,

    /// The guest has nothing to spare.
    NoHeadroom,

    /// The reclaim left nothing the guest can spare.
    Spent,

    /// The workload throttled before the reclaim landed.
    Braked,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Growing => "a grant request is outstanding",
            Self::Reclaiming => "the workload is reclaiming to make room",
            Self::Releasing => "a release is already in progress",
            Self::Exhausted => "the workload is stalled at its ceiling",
            Self::ReclaimPending => "a reclaim has yet to report",
            Self::NoHeadroom => "no headroom to release without inducing pressure",
            Self::Spent => "the reclaim left nothing the guest can spare",
            Self::Braked => "the workload throttled before the reclaim landed",
        })
    }
}
