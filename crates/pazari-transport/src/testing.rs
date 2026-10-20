use std::{
    path::PathBuf,
    sync::atomic::{AtomicU32, Ordering},
};

pub use pazari_proto::testing::{guest_messages, host_messages};
use pazari_proto::{Codec, JsonLinesCodec};

pub use crate::{GuestStream, HostStream};

static SEQUENCE: AtomicU32 = AtomicU32::new(0);

pub fn socket_path(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "pazari-transport-{}-{name}-{}.sock",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    path
}

pub fn framed<M: serde::Serialize>(messages: &[M]) -> Vec<u8> {
    let mut out = Vec::new();
    for message in messages {
        JsonLinesCodec::encode(message, &mut out).expect("failed to encode a message");
    }
    out
}
