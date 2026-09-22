//! The labels the pazari binaries split their series by.

use prometheus_client::encoding::EncodeLabelSet;

/// One guest.
#[derive(Debug, Clone, PartialEq, Eq, Hash, EncodeLabelSet)]
pub struct GuestLabel {
    pub guest: String,
}

/// The reason a request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EncodeLabelSet)]
pub struct ReasonLabel {
    pub reason: &'static str,
}

/// Which of a tagged set a series counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EncodeLabelSet)]
pub struct KindLabel {
    pub kind: &'static str,
}

/// Which state the agent is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EncodeLabelSet)]
pub struct ModeLabel {
    pub mode: &'static str,
}
