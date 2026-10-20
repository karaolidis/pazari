use pazari_proto::GuestMessage;
use pazari_transport::PeerId;
use pazari_types::Timestamp;

use crate::broker::registry::GuestId;

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Connected {
        peer: PeerId,
    },

    Message {
        now: Timestamp,
        peer: PeerId,
        message: GuestMessage,
    },

    Disconnected {
        peer: PeerId,
    },

    Size {
        now: Timestamp,
        guest: GuestId,
        size: u64,
        hotplugged: u64,
    },

    HostAvailable {
        bytes: u64,
        swap_free: u64,

        /// Absent where the kernel carries no `/proc/pressure/memory`.
        psi: Option<f64>,
    },

    Tick {
        now: Timestamp,
    },
}
