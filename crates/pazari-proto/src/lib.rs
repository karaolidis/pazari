//! Wire protocol for pazari guest-host negotiation.
//!
//! See `docs/protocol.md` for the normative specification.

pub mod codec;
pub mod error;
pub mod message;

pub use codec::{Codec, Decoder, JsonLinesCodec, JsonLinesDecoder};
pub use error::{DecodeError, EncodeError};
pub use message::{DenyReason, GrantReason, GuestMessage, HostMessage, Urgency};

pub const PROTOCOL_VERSION: u16 = 1;

pub const MAX_LINE_BYTES: usize = 64 * 1024;

pub const DEFAULT_VSOCK_PORT: u32 = 4650;
