use std::{marker::PhantomData, str};

use serde::{Serialize, de::DeserializeOwned};

use crate::{
    DecodeError, EncodeError,
    codec::{Codec, Decoder},
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JsonLinesCodec;

impl Codec for JsonLinesCodec {
    type Decoder<M: DeserializeOwned> = JsonLinesDecoder<M>;

    const MAX_FRAME_BYTES: usize = 64 * 1024;

    fn encode<M>(message: &M, out: &mut Vec<u8>) -> Result<(), EncodeError>
    where
        M: Serialize + ?Sized,
    {
        let start = out.len();

        if let Err(error) = serde_json::to_writer(&mut *out, message) {
            out.truncate(start);
            return Err(error.into());
        }

        let len = out.len() - start;
        if len > Self::MAX_FRAME_BYTES {
            out.truncate(start);
            return Err(EncodeError::LineTooLong {
                len,
                cap: Self::MAX_FRAME_BYTES,
            });
        }

        out.push(b'\n');
        Ok(())
    }

    fn decoder<M: DeserializeOwned>() -> Self::Decoder<M> {
        JsonLinesDecoder::new()
    }
}

#[derive(Debug)]
pub struct JsonLinesDecoder<M> {
    buf: Vec<u8>,
    poisoned: bool,
    message: PhantomData<fn() -> M>,
}

impl<M> JsonLinesDecoder<M> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buf: Vec::new(),
            poisoned: false,
            message: PhantomData,
        }
    }
}

impl<M> Default for JsonLinesDecoder<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: DeserializeOwned> Decoder<M> for JsonLinesDecoder<M> {
    fn push(&mut self, chunk: &[u8]) -> Result<(), DecodeError> {
        if self.poisoned {
            return Err(DecodeError::Poisoned);
        }

        self.buf.extend_from_slice(chunk);

        let unterminated = self
            .buf
            .iter()
            .rposition(|&byte| byte == b'\n')
            .map_or(self.buf.len(), |index| self.buf.len() - index - 1);

        if unterminated > JsonLinesCodec::MAX_FRAME_BYTES {
            self.poisoned = true;
            return Err(DecodeError::LineTooLong {
                len: unterminated,
                cap: JsonLinesCodec::MAX_FRAME_BYTES,
            });
        }

        Ok(())
    }

    fn next_message(&mut self) -> Result<Option<M>, DecodeError> {
        if self.poisoned {
            return Err(DecodeError::Poisoned);
        }

        let Some(index) = self.buf.iter().position(|&byte| byte == b'\n') else {
            return Ok(None);
        };

        let result = decode_line(&self.buf[..index]);
        self.buf.drain(..=index);

        match result {
            Ok(message) => Ok(Some(message)),
            Err(error) => {
                self.poisoned = true;
                Err(error)
            }
        }
    }

    fn pending(&self) -> usize {
        self.buf.len()
    }

    fn is_poisoned(&self) -> bool {
        self.poisoned
    }
}

fn decode_line<M: DeserializeOwned>(line: &[u8]) -> Result<M, DecodeError> {
    if line.is_empty() {
        return Err(DecodeError::EmptyLine);
    }

    if line.len() > JsonLinesCodec::MAX_FRAME_BYTES {
        return Err(DecodeError::LineTooLong {
            len: line.len(),
            cap: JsonLinesCodec::MAX_FRAME_BYTES,
        });
    }

    if let Some(offset) = line.iter().position(|&byte| byte == b'\r') {
        return Err(DecodeError::CarriageReturn {
            offset,
            len: line.len(),
        });
    }

    let text = str::from_utf8(line)?;

    serde_json::from_str(text).map_err(|source| DecodeError::Malformed {
        snippet: snippet(text),
        source,
    })
}

const SNIPPET_CHARS: usize = 256;

fn snippet(line: &str) -> String {
    match line.char_indices().nth(SNIPPET_CHARS) {
        Some((offset, _)) => format!("{}...", &line[..offset]),
        None => line.to_owned(),
    }
}
