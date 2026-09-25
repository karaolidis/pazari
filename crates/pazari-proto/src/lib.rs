//! Wire protocol for pazari guest-host negotiation.

pub mod codec;
pub mod error;
pub mod message;

pub use codec::{Codec, Decoder, JsonLinesCodec, JsonLinesDecoder};
pub use error::{DecodeError, EncodeError};
pub use message::{DenyReason, Guest, GuestMessage, Host, HostMessage, Peer, Status};

pub const PROTOCOL_VERSION: u16 = 1;

pub const DEFAULT_VSOCK_PORT: u32 = 4650;
