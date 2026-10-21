use std::marker::PhantomData;

use pazari_proto::{Codec, Decoder, Guest, Host, JsonLinesCodec, Peer};
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

/// A guest-side stream; sends [`GuestMessage`], receives [`HostMessage`].
///
/// [`GuestMessage`]: pazari_proto::GuestMessage
/// [`HostMessage`]: pazari_proto::HostMessage
pub type GuestStream<T> = MessageStream<T, JsonLinesCodec, Guest>;

/// A host-side stream; sends [`HostMessage`], receives [`GuestMessage`].
///
/// [`GuestMessage`]: pazari_proto::GuestMessage
/// [`HostMessage`]: pazari_proto::HostMessage
pub type HostStream<T> = MessageStream<T, JsonLinesCodec, Host>;

#[cfg(test)]
mod tests {
    use pazari_proto::GuestMessage;
    use tokio::io::{AsyncWriteExt, DuplexStream, duplex};

    use crate::{
        TransportError,
        testing::{GuestStream, HostStream, framed, guest_messages, host_messages},
    };

    const BUFFER: usize = 1 << 20;

    fn pair() -> (DuplexStream, DuplexStream) {
        duplex(BUFFER)
    }

    #[tokio::test]
    async fn messages_travel_in_both_directions() {
        let (guest, host) = pair();
        let mut guest = GuestStream::new(guest);
        let mut host = HostStream::new(host);

        for message in &guest_messages() {
            guest.send(message).await.unwrap();
        }

        let mut received = Vec::new();
        for _ in 0..guest_messages().len() {
            received.push(host.recv().await.unwrap().unwrap());
        }
        assert_eq!(received, guest_messages());

        for message in &host_messages() {
            host.send(message).await.unwrap();
        }

        let mut received = Vec::new();
        for _ in 0..host_messages().len() {
            received.push(guest.recv().await.unwrap().unwrap());
        }
        assert_eq!(received, host_messages());
    }

    #[tokio::test]
    async fn a_message_split_into_small_writes_arrives_whole() {
        let message = guest_messages().swap_remove(2);
        let wire = framed(std::slice::from_ref(&message));

        let (mut guest, host) = duplex(3);

        let writer = tokio::spawn(async move {
            guest.write_all(&wire).await.unwrap();
            guest.flush().await.unwrap();
        });

        let mut stream = HostStream::new(host);

        assert_eq!(stream.recv().await.unwrap(), Some(message));

        writer.await.unwrap();
        assert_eq!(stream.recv().await.unwrap(), None);
    }

    #[tokio::test]
    async fn messages_written_in_one_burst_arrive_in_order() {
        let messages = guest_messages();
        let wire = framed(&messages);

        let (mut guest, host) = pair();
        guest.write_all(&wire).await.unwrap();
        guest.flush().await.unwrap();

        let mut stream = HostStream::new(host);

        let mut received = Vec::new();
        for _ in 0..messages.len() {
            received.push(stream.recv().await.unwrap().unwrap());
        }

        assert_eq!(received, messages);
    }

    #[tokio::test]
    async fn a_clean_close_ends_the_stream() {
        let (guest, host) = pair();

        let mut guest = GuestStream::new(guest);
        guest.send(&GuestMessage::Bye {}).await.unwrap();
        drop(guest);

        let mut stream = HostStream::new(host);

        assert_eq!(stream.recv().await.unwrap(), Some(GuestMessage::Bye {}));
        assert_eq!(stream.recv().await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_close_mid_message_is_truncated() {
        let wire = framed(std::slice::from_ref(&GuestMessage::Bye {}));
        let partial = wire.len() - 2;

        let (mut guest, host) = pair();
        guest.write_all(&wire[..partial]).await.unwrap();
        guest.flush().await.unwrap();
        drop(guest);

        let mut stream = HostStream::new(host);

        let error = stream
            .recv()
            .await
            .expect_err("expected a truncated stream");
        assert!(
            matches!(error, TransportError::Truncated { pending } if pending == partial),
            "unexpected error: {error}"
        );
    }
}
