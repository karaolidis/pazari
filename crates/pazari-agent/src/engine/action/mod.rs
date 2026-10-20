use pazari_proto::GuestMessage;
use pazari_types::Swappiness;

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    SetHigh(u64),
    SetMax(u64),
    SetOomGroup(bool),

    StartReclaim {
        bytes: u64,
        swappiness: Option<Swappiness>,
    },

    CancelReclaim,

    Send(GuestMessage),
}
