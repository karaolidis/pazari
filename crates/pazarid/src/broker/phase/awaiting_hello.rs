use std::num::NonZeroU64;

use pazari_proto::{HostMessage, PROTOCOL_VERSION};
use tracing::{info, warn};

use crate::broker::phase::{Negotiated, Phase, State, Step};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Held {
    block_size: NonZeroU64,

    /// What the guest read as `MemTotal` when it said hello.
    total_mem: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AwaitingHello {
    held: Option<Held>,
}

impl AwaitingHello {
    #[must_use]
    pub const fn new() -> Self {
        Self { held: None }
    }

    /// Absent while there is no hello to answer or the device has not reported.
    pub fn welcome(self, step: &mut Step<'_>) -> Option<Negotiated> {
        let held = self.held?;
        let ceiling = step.ceiling();
        let hotplugged = step.geometry()?.hotplugged;

        step.world.observe_total_mem(step.guest, held.total_mem);

        info!(
            guest = step.name(),
            peer = %step.peer,
            ceiling = ceiling.get(),
            hotplugged,
            "sending welcome",
        );

        let status_interval = step.policy().status_interval.get();

        step.send(HostMessage::Welcome {
            protocol: PROTOCOL_VERSION,
            ceiling: ceiling.get(),
            status_interval,
            hotplugged,
        });

        Some(Negotiated::new(held.block_size, step.now))
    }

    fn reject(self, step: &mut Step<'_>) -> Phase {
        step.close();

        self.stay()
    }
}

impl State for AwaitingHello {
    fn stay(self) -> Phase {
        Phase::AwaitingHello(self)
    }

    fn refuse(self, step: &mut Step<'_>, what: &'static str) -> Phase {
        warn!(
            guest = step.name(),
            peer = %step.peer,
            said = what,
            "guest sent a message before hello",
        );

        self.reject(step)
    }

    fn on_hello(
        self,
        step: &mut Step<'_>,
        protocol: u16,
        agent_version: &str,
        block_size: NonZeroU64,
        total_mem: u64,
    ) -> Phase {
        if self.held.is_some() {
            warn!(guest = step.name(), peer = %step.peer, "duplicate hello");

            return self.reject(step);
        }

        if protocol != PROTOCOL_VERSION {
            warn!(
                guest = step.name(),
                peer = %step.peer,
                theirs = protocol,
                ours = PROTOCOL_VERSION,
                "guest protocol version mismatch",
            );

            return self.reject(step);
        }

        if total_mem == 0 {
            warn!(
                guest = step.name(),
                peer = %step.peer,
                block_size,
                total_mem,
                "guest declared an impossible geometry",
            );

            return self.reject(step);
        }

        info!(
            guest = step.name(),
            peer = %step.peer,
            agent_version,
            block_size,
            total_mem,
            "guest sent hello",
        );

        let held = Self {
            held: Some(Held {
                block_size,
                total_mem,
            }),
        };

        held.welcome(step).map_or_else(
            || {
                info!(
                    guest = step.name(),
                    peer = %step.peer,
                    "delaying welcome until the memory device reports",
                );

                held.stay()
            },
            Phase::Negotiated,
        )
    }
}
