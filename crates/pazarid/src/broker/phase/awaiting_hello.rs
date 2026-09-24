use pazari_proto::{GuestMessage, HostMessage, PROTOCOL_VERSION};
use pazari_transport::PeerId;
use pazari_types::Timestamp;
use tracing::{info, warn};

use crate::broker::{
    context::Context,
    phase::{Negotiated, Phase, Said, State},
    registry::GuestId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Held {
    block_size: u64,
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

    pub fn welcome(
        self,
        ctx: &mut Context,
        now: Timestamp,
        guest: GuestId,
        peer: &PeerId,
    ) -> Phase {
        let Some(held) = self.held else {
            return self.stay();
        };

        let Some(ceiling) = ctx.ceiling(guest) else {
            return self.stay();
        };

        let Some(hotplugged) = ctx.hotplugged(guest) else {
            return self.stay();
        };

        info!(
            guest = ctx.name(guest),
            %peer,
            ceiling = ceiling.get(),
            hotplugged,
            "sending welcome",
        );

        ctx.send(
            peer,
            HostMessage::Welcome {
                protocol: PROTOCOL_VERSION,
                ceiling: ceiling.get(),
                status_interval_ms: ctx.policy.welcome_status_interval.get(),
                hotplugged,
            },
        );

        Phase::Negotiated(Negotiated::new(held.block_size, now))
    }
}

impl State for AwaitingHello {
    fn stay(self) -> Phase {
        Phase::AwaitingHello(self)
    }

    fn refuse(self, ctx: &mut Context, said: Said<'_>, what: &'static str) -> Phase {
        warn!(
            guest = ctx.name(said.guest),
            peer = %said.peer,
            said = what,
            "guest sent a message before hello",
        );

        ctx.close(said.guest, said.peer);

        self.stay()
    }

    fn on_hello(self, ctx: &mut Context, said: Said<'_>, message: GuestMessage) -> Phase {
        let GuestMessage::Hello {
            protocol,
            agent_version,
            block_size,
            total_mem,
        } = message
        else {
            return self.refuse(ctx, said, "something other than hello");
        };

        if self.held.is_some() {
            warn!(
                guest = ctx.name(said.guest),
                peer = %said.peer,
                "duplicate hello",
            );
            ctx.close(said.guest, said.peer);

            return self.stay();
        }

        if protocol != PROTOCOL_VERSION {
            warn!(
                guest = ctx.name(said.guest),
                peer = %said.peer,
                theirs = protocol,
                ours = PROTOCOL_VERSION,
                "guest protocol version mismatch",
            );
            ctx.close(said.guest, said.peer);

            return self.stay();
        }

        if block_size == 0 || total_mem == 0 {
            warn!(
                guest = ctx.name(said.guest),
                peer = %said.peer,
                block_size,
                total_mem,
                "guest declared an impossible geometry",
            );
            ctx.close(said.guest, said.peer);

            return self.stay();
        }

        if ctx.ceiling(said.guest).is_none() {
            warn!(
                peer = %said.peer,
                "hello from an unregistered guest",
            );
            ctx.close(said.guest, said.peer);

            return self.stay();
        }

        ctx.observe_total_mem(said.guest, total_mem);

        info!(
            guest = ctx.name(said.guest),
            peer = %said.peer,
            agent_version,
            block_size,
            total_mem,
            "guest sent hello",
        );

        let held = Self {
            held: Some(Held { block_size }),
        };

        let next = held.welcome(ctx, said.now, said.guest, said.peer);

        if matches!(next, Phase::AwaitingHello(_)) {
            info!(
                guest = ctx.name(said.guest),
                peer = %said.peer,
                "delaying welcome until the memory device reports",
            );
        }

        next
    }
}
