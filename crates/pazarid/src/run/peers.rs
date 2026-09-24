use std::collections::HashMap;

use pazari_proto::{GuestMessage, HostMessage};
use pazari_transport::{self as transport, AnyConnection, Ended, HostStream, PeerId};
use pazari_types::Timestamp;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::broker::Event;

const OUTBOUND_DEPTH: usize = 64;
const REPORT_DEPTH: usize = 256;

type Stream = HostStream<AnyConnection>;

#[derive(Debug)]
enum Said {
    Message(GuestMessage),

    Gone,
}

#[derive(Debug)]
pub struct Report {
    link: u64,
    peer: PeerId,
    said: Said,
}

#[derive(Debug)]
struct Link {
    id: u64,
    outbound: mpsc::Sender<HostMessage>,
}

#[derive(Debug)]
pub struct Peers {
    links: HashMap<PeerId, Link>,
    reports: mpsc::Sender<Report>,
    next: u64,
}

impl Peers {
    #[must_use]
    pub fn new() -> (Self, mpsc::Receiver<Report>) {
        let (reports, queue) = mpsc::channel(REPORT_DEPTH);

        let peers = Self {
            links: HashMap::new(),
            reports,
            next: 0,
        };

        (peers, queue)
    }

    pub fn accept(&mut self, connection: AnyConnection, peer: PeerId) {
        let (outbound, outgoing) = mpsc::channel(OUTBOUND_DEPTH);

        self.next = self.next.saturating_add(1);
        let id = self.next;

        tokio::spawn(pump(
            Stream::new(connection),
            id,
            peer.clone(),
            self.reports.clone(),
            outgoing,
        ));

        if self.links.insert(peer, Link { id, outbound }).is_some() {
            debug!("dropping a connection the peer replaced");
        }
    }

    pub fn digest(&mut self, report: Report, now: Timestamp) -> Option<Event> {
        if self.links.get(&report.peer).map(|link| link.id) != Some(report.link) {
            return None;
        }

        match report.said {
            Said::Message(message) => Some(Event::Message {
                now,
                peer: report.peer,
                message,
            }),
            Said::Gone => {
                self.links.remove(&report.peer);

                Some(Event::Disconnected {
                    now,
                    peer: report.peer,
                })
            }
        }
    }

    pub fn send(&self, peer: &PeerId, message: HostMessage) {
        let Some(link) = self.links.get(peer) else {
            warn!(%peer, ?message, "no connection; dropping a message");
            return;
        };

        if let Err(error) = link.outbound.try_send(message) {
            warn!(%peer, %error, "connection queue is full; dropping a message");
        }
    }

    pub fn close(&mut self, peer: &PeerId) {
        if self.links.remove(peer).is_some() {
            debug!(%peer, "closing a connection");
        }
    }

    pub fn clear(&mut self) {
        self.links.clear();
    }
}

async fn pump(
    mut stream: Stream,
    link: u64,
    peer: PeerId,
    reports: mpsc::Sender<Report>,
    mut outgoing: mpsc::Receiver<HostMessage>,
) {
    let tagged = {
        let peer = peer.clone();

        move |message| Report {
            link,
            peer: peer.clone(),
            said: Said::Message(message),
        }
    };

    match transport::pump(&mut stream, &reports, tagged, &mut outgoing).await {
        Ended::Closed => debug!(%peer, "guest closed the connection"),
        Ended::Recv(error) => warn!(%peer, %error, "connection failed"),
        Ended::Send(error) => warn!(%peer, %error, "failed to send to the guest"),
        // The daemon let go of this connection; nobody is left to tell.
        Ended::Local => return,
    }

    let report = Report {
        link,
        peer,
        said: Said::Gone,
    };

    drop(reports.send(report).await);
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pazari_proto::PROTOCOL_VERSION;
    use pazari_transport::{AnyConnector, Connector, Listener, UnixConnector, UnixListener};
    use tempfile::TempDir;
    use tokio::time::timeout;

    use super::*;

    const PATIENCE: Duration = Duration::from_secs(5);

    type GuestStream = pazari_transport::GuestStream<AnyConnection>;

    fn at(millis: u64) -> Timestamp {
        Timestamp::from_millis(millis)
    }

    fn hello() -> GuestMessage {
        GuestMessage::Hello {
            protocol: PROTOCOL_VERSION,
            agent_version: "0.1.0".to_owned(),
            block_size: 1 << 27,
            total_mem: 1 << 31,
        }
    }

    async fn join(
        listener: &UnixListener,
        connector: &AnyConnector,
    ) -> (AnyConnection, PeerId, GuestStream) {
        let guest = connector.connect().await.expect("a connection");
        let (connection, peer) = timeout(PATIENCE, listener.accept())
            .await
            .expect("the guest dialled in time")
            .expect("a connection");

        (
            AnyConnection::Unix(connection),
            peer,
            GuestStream::new(guest),
        )
    }

    async fn next(reports: &mut mpsc::Receiver<Report>) -> Report {
        timeout(PATIENCE, reports.recv())
            .await
            .expect("a connection spoke in time")
            .expect("the daemon still holds a sender")
    }

    #[tokio::test]
    async fn what_a_guest_says_becomes_a_message_event() {
        let dir = TempDir::new().expect("a temporary directory");
        let path = dir.path().join("host.sock");

        let listener = UnixListener::bind(&path).expect("a bound listener");
        let connector = AnyConnector::Unix(UnixConnector::new(&path));

        let (mut peers, mut reports) = Peers::new();
        let (connection, peer, mut guest) = join(&listener, &connector).await;

        peers.accept(connection, peer.clone());

        guest.send(&hello()).await.expect("a writable connection");

        assert_eq!(
            peers.digest(next(&mut reports).await, at(1)),
            Some(Event::Message {
                now: at(1),
                peer: peer.clone(),
                message: hello(),
            })
        );

        peers.send(&peer, HostMessage::Ceiling { ceiling: 42 });

        assert_eq!(
            timeout(PATIENCE, guest.recv())
                .await
                .expect("the daemon answered in time")
                .expect("a readable connection"),
            Some(HostMessage::Ceiling { ceiling: 42 })
        );
    }

    #[tokio::test]
    async fn a_guest_that_hangs_up_disconnects_once() {
        let dir = TempDir::new().expect("a temporary directory");
        let path = dir.path().join("host.sock");

        let listener = UnixListener::bind(&path).expect("a bound listener");
        let connector = AnyConnector::Unix(UnixConnector::new(&path));

        let (mut peers, mut reports) = Peers::new();
        let (connection, peer, guest) = join(&listener, &connector).await;

        peers.accept(connection, peer.clone());
        drop(guest);

        assert_eq!(
            peers.digest(next(&mut reports).await, at(2)),
            Some(Event::Disconnected {
                now: at(2),
                peer: peer.clone(),
            })
        );

        peers.send(&peer, HostMessage::Ceiling { ceiling: 42 });
    }

    #[tokio::test]
    async fn a_connection_that_arrives_over_a_live_one_buries_its_word() {
        let dir = TempDir::new().expect("a temporary directory");
        let path = dir.path().join("host.sock");

        let listener = UnixListener::bind(&path).expect("a bound listener");
        let connector = AnyConnector::Unix(UnixConnector::new(&path));

        let (mut peers, mut reports) = Peers::new();

        let (first, peer, mut stale) = join(&listener, &connector).await;
        peers.accept(first, peer.clone());

        stale.send(&hello()).await.expect("a writable connection");

        let (second, _, mut fresh) = join(&listener, &connector).await;
        peers.accept(second, peer.clone());

        fresh
            .send(&GuestMessage::Bye {})
            .await
            .expect("a writable connection");

        let mut digested = Vec::new();
        while digested.is_empty() {
            if let Some(event) = peers.digest(next(&mut reports).await, at(3)) {
                digested.push(event);
            }
        }

        assert_eq!(
            digested,
            vec![Event::Message {
                now: at(3),
                peer,
                message: GuestMessage::Bye {},
            }],
            "the replaced connection still spoke for the peer"
        );
    }

    #[tokio::test]
    async fn a_closed_connection_is_let_go_of() {
        let dir = TempDir::new().expect("a temporary directory");
        let path = dir.path().join("host.sock");

        let listener = UnixListener::bind(&path).expect("a bound listener");
        let connector = AnyConnector::Unix(UnixConnector::new(&path));

        let (mut peers, _reports) = Peers::new();
        let (connection, peer, mut guest) = join(&listener, &connector).await;

        peers.accept(connection, peer.clone());
        peers.close(&peer);

        assert_eq!(
            timeout(PATIENCE, guest.recv())
                .await
                .expect("the daemon let go in time")
                .expect("a readable connection"),
            None
        );

        peers.clear();
    }
}
