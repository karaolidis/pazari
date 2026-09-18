use std::marker::PhantomData;

use pazari_proto::{Codec, Decoder};
use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{Connection, TransportError};

const READ_CHUNK_BYTES: usize = 8 * 1024;

pub struct MessageStream<T, C, In, Out>
where
    C: Codec,
    In: DeserializeOwned,
{
    connection: T,
    decoder: C::Decoder<In>,
    chunk: Box<[u8]>,
    encoded: Vec<u8>,
    outgoing: PhantomData<fn(&Out)>,
}

impl<T, C, In, Out> MessageStream<T, C, In, Out>
where
    T: Connection,
    C: Codec,
    In: DeserializeOwned,
    Out: Serialize,
{
    #[must_use]
    pub fn new(connection: T) -> Self {
        Self {
            connection,
            decoder: C::decoder::<In>(),
            chunk: vec![0; READ_CHUNK_BYTES].into_boxed_slice(),
            encoded: Vec::new(),
            outgoing: PhantomData,
        }
    }

    pub async fn send(&mut self, message: &Out) -> Result<(), TransportError> {
        self.encoded.clear();
        C::encode(message, &mut self.encoded)?;

        self.connection.write_all(&self.encoded).await?;
        self.connection.flush().await?;

        Ok(())
    }

    pub async fn recv(&mut self) -> Result<Option<In>, TransportError> {
        loop {
            if let Some(message) = self.decoder.next_message()? {
                return Ok(Some(message));
            }

            let read = self.connection.read(&mut self.chunk).await?;

            if read == 0 {
                let pending = self.decoder.pending();

                return if pending == 0 {
                    Ok(None)
                } else {
                    Err(TransportError::Truncated { pending })
                };
            }

            self.decoder.push(&self.chunk[..read])?;
        }
    }

    pub fn into_inner(self) -> T {
        self.connection
    }
}
