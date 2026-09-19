use std::{num::NonZeroU64, time::Duration};

use pazari_proto::HostMessage;

use crate::engine::time::Timestamp;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootInfo {
    pub boot_mem: NonZeroU64,
    pub block_size: NonZeroU64,
    pub plugged: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Snapshot {
    pub plugged: u64,
    pub workload_usage: u64,
    pub available: u64,
    pub swap_used: u64,
    pub high_events: u64,
    pub max_events: u64,
    pub psi_some10: f32,
    pub psi_full10: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Tick {
        now: Timestamp,
        snapshot: Snapshot,
    },

    ThrottleWake {
        now: Timestamp,
        high_events: u64,
        max_events: u64,
    },

    PluggedChanged {
        now: Timestamp,
        plugged: u64,
    },

    ReclaimFinished {
        now: Timestamp,
        reclaimed: u64,
    },

    Connected {
        now: Timestamp,
        ceiling: u64,
        status_interval: Duration,
    },

    Disconnected {
        now: Timestamp,
    },

    Host {
        now: Timestamp,
        message: HostMessage,
    },
}
