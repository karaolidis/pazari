//! Wire protocol for pazari guest-host negotiation.

pub mod codec;
pub mod error;
pub mod message;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use codec::{Codec, Decoder, JsonLinesCodec, JsonLinesDecoder};
pub use error::{DecodeError, EncodeError};
pub use message::{
    DenyReason, Guest, GuestMessage, Host, HostMessage, Peer, ReclaimId, RequestId, Status,
};

pub const PROTOCOL_VERSION: u16 = 1;
