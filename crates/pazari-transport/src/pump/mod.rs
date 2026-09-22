use pazari_proto::{Codec, Peer};
use tokio::sync::mpsc;

use crate::{Connection, MessageStream, TransportError};

/// Why a [`pump`] stopped.
#[derive(Debug)]
pub enum Ended {
    /// The peer closed the connection.
    Closed,

    /// The connection failed while reading.
    Recv(TransportError),

    /// The connection failed while writing.
    Send(TransportError),

    /// This side went away: the sink was dropped, or there is nothing left to
    /// send.
    Local,
}

#[derive(Debug)]
enum Step<P: Peer> {
    Incoming(Result<Option<P::Incoming>, TransportError>),
    Outgoing(Option<P::Outgoing>),
}

/// Carries messages both ways over one connection until it ends.
pub async fn pump<T, C, P, M>(
    stream: &mut MessageStream<T, C, P>,
    incoming: &mpsc::Sender<M>,
    mut wrap: impl FnMut(P::Incoming) -> M,
    outgoing: &mut mpsc::Receiver<P::Outgoing>,
) -> Ended
where
    T: Connection,
    C: Codec,
    P: Peer,
{
    loop {
        let step = tokio::select! {
            incoming = stream.recv() => Step::<P>::Incoming(incoming),
            message = outgoing.recv() => Step::<P>::Outgoing(message),
        };

        match step {
            Step::Incoming(Ok(Some(message))) => {
                if incoming.send(wrap(message)).await.is_err() {
                    return Ended::Local;
                }
            }
            Step::Incoming(Ok(None)) => return Ended::Closed,
            Step::Incoming(Err(error)) => return Ended::Recv(error),
            Step::Outgoing(Some(message)) => {
                if let Err(error) = stream.send(&message).await {
                    return Ended::Send(error);
                }
            }
            Step::Outgoing(None) => return Ended::Local,
        }
    }
}
