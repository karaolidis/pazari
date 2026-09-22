//! Transport abstractions and adapters for pazari.

pub mod adapter;
pub mod connection;
pub mod error;
pub mod message;
pub mod pump;

#[cfg(test)]
mod testing;

pub use adapter::{
    AnyConnection, AnyConnector, AnyListener, UnixConnector, UnixListener, VsockConnector,
    VsockListener,
};
pub use connection::{Connection, Connector, Listener, PeerId};
pub use error::TransportError;
pub use message::{GuestStream, HostStream, MessageStream};
pub use pump::{Ended, pump};
