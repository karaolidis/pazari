use pazari_proto::GuestMessage;
use pazari_transport::PeerId;
use pazari_types::Timestamp;

use crate::broker::registry::GuestId;

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Connected {
        now: Timestamp,
        peer: PeerId,
    },

    Message {
        now: Timestamp,
        peer: PeerId,
        message: GuestMessage,
    },

    Disconnected {
        now: Timestamp,
        peer: PeerId,
    },

    Size {
        now: Timestamp,
        guest: GuestId,
        size: u64,
        hotplugged: u64,
    },

    HostAvailable {
        now: Timestamp,
        bytes: u64,
        swap_free: u64,

        /// Absent where the kernel carries no `/proc/pressure/memory`.
        psi: Option<f64>,
    },

    Reclaim {
        now: Timestamp,
        guest: GuestId,
        desired: u64,
    },

    Tick {
        now: Timestamp,
    },
}

impl Event {
    #[must_use]
    pub const fn now(&self) -> Timestamp {
        match *self {
            Self::Connected { now, .. }
            | Self::Message { now, .. }
            | Self::Disconnected { now, .. }
            | Self::Size { now, .. }
            | Self::HostAvailable { now, .. }
            | Self::Reclaim { now, .. }
            | Self::Tick { now } => now,
        }
    }
}
