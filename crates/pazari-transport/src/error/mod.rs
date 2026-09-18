use std::io;

use pazari_proto::{DecodeError, EncodeError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("transport i/o failed: {source}")]
    Io {
        #[from]
        source: io::Error,
    },

    #[error("failed to encode an outgoing message: {source}")]
    Encode {
        #[from]
        source: EncodeError,
    },

    #[error("failed to decode an incoming message: {source}")]
    Decode {
        #[from]
        source: DecodeError,
    },

    #[error("peer closed with {pending} bytes of an unterminated message")]
    Truncated { pending: usize },
}
