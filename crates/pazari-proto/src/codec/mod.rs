pub mod json_lines;

pub use json_lines::{JsonLinesCodec, JsonLinesDecoder};

use serde::{Serialize, de::DeserializeOwned};

use crate::{DecodeError, EncodeError};

pub trait Codec {
    type Decoder<M: DeserializeOwned>: Decoder<M>;

    fn encode<M>(message: &M, out: &mut Vec<u8>) -> Result<(), EncodeError>
    where
        M: Serialize + ?Sized;

    fn encode_to_vec<M>(message: &M) -> Result<Vec<u8>, EncodeError>
    where
        M: Serialize + ?Sized,
    {
        let mut out = Vec::new();
        Self::encode(message, &mut out)?;
        Ok(out)
    }

    fn decoder<M: DeserializeOwned>() -> Self::Decoder<M>;
}

pub trait Decoder<M> {
    fn push(&mut self, chunk: &[u8]) -> Result<(), DecodeError>;

    fn next_message(&mut self) -> Result<Option<M>, DecodeError>;

    fn pending(&self) -> usize;
}
