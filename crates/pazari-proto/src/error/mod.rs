use std::str::Utf8Error;

use thiserror::Error;

use crate::MAX_LINE_BYTES;

#[derive(Debug, Error)]
pub enum EncodeError {
    #[error("failed to serialize message: {0}")]
    Serialize(#[from] serde_json::Error),

    #[error("encoded message is {len} bytes, over MAX_LINE_BYTES ({MAX_LINE_BYTES})")]
    LineTooLong { len: usize },
}

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("line reached {len} bytes with no terminator, over MAX_LINE_BYTES ({MAX_LINE_BYTES})")]
    LineTooLong { len: usize },

    #[error("empty line")]
    EmptyLine,

    #[error("carriage return at byte {offset} of a {len} byte line")]
    CarriageReturn { offset: usize, len: usize },

    #[error("line is not valid UTF-8: {source}")]
    NotUtf8 {
        #[from]
        source: Utf8Error,
    },

    #[error("malformed message: {source}; line: {snippet}")]
    Malformed {
        snippet: String,
        source: serde_json::Error,
    },

    #[error("decoder was poisoned by an earlier protocol violation")]
    Poisoned,
}
