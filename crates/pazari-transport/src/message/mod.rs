use std::marker::PhantomData;

use pazari_proto::{Codec, Decoder, Peer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{Connection, TransportError};

const READ_CHUNK_BYTES: usize = 8 * 1024;

pub struct MessageStream<T, C, P>
where
    C: Codec,
    P: Peer,
{
    connection: T,
    decoder: C::Decoder<P::Incoming>,
    chunk: Box<[u8]>,
    encoded: Vec<u8>,
    peer: PhantomData<fn() -> P>,
}

impl<T, C, P> MessageStream<T, C, P>
where
    T: Connection,
    C: Codec,
    P: Peer,
{
    #[must_use]
    pub fn new(connection: T) -> Self {
        Self {
            connection,
            decoder: C::decoder::<P::Incoming>(),
            chunk: vec![0; READ_CHUNK_BYTES].into_boxed_slice(),
            encoded: Vec::new(),
            peer: PhantomData,
        }
    }

    pub async fn send(&mut self, message: &P::Outgoing) -> Result<(), TransportError> {
        self.encoded.clear();
        C::encode(message, &mut self.encoded)?;

        self.connection.write_all(&self.encoded).await?;
        self.connection.flush().await?;

        Ok(())
    }

    pub async fn recv(&mut self) -> Result<Option<P::Incoming>, TransportError> {
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
