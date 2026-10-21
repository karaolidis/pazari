/// A transition the engine counts for whoever watches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// The workload wanted more than the ceiling allows.
    Exhaustion,

    /// A throttle was answered by reclaiming.
    ThrottleReclaim,

    /// A throttle reclaim fell short. The guest asked the host instead.
    ThrottleReclaimShortfall,
}
