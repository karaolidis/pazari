use pazari_proto::HostMessage;
use pazari_transport::PeerId;

use crate::broker::registry::GuestId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Send { peer: PeerId, message: HostMessage },

    Close { peer: PeerId },

    SetRequested { guest: GuestId, bytes: u64 },
}
