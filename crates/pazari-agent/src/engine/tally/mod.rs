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

/// How many of each transition the engine has made since it started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Tally {
    pub exhaustions: u64,
    pub throttle_reclaims: u64,
    pub throttle_reclaim_shortfalls: u64,
}

impl Tally {
    pub const fn record(&mut self, transition: Transition) {
        let count = match transition {
            Transition::Exhaustion => &mut self.exhaustions,
            Transition::ThrottleReclaim => &mut self.throttle_reclaims,
            Transition::ThrottleReclaimShortfall => &mut self.throttle_reclaim_shortfalls,
        };

        *count = count.saturating_add(1);
    }
}
