use pazari_proto::{GuestMessage, HostMessage, PROTOCOL_VERSION};
use tracing::{info, warn};

use crate::broker::{
    context::Context,
    phase::{Negotiated, Phase, Said, State},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AwaitingHello;

impl State for AwaitingHello {
    fn stay(self) -> Phase {
        Phase::AwaitingHello(self)
    }

    fn refuse(self, ctx: &mut Context, said: Said<'_>, what: &'static str) -> Phase {
        warn!(
            guest = ctx.name(said.guest),
            peer = %said.peer,
            said = what,
            "the guest spoke before it said hello",
        );

        ctx.close(said.guest, said.peer);

        self.stay()
    }

    fn on_hello(self, ctx: &mut Context, said: Said<'_>, message: GuestMessage) -> Phase {
        let GuestMessage::Hello {
            protocol,
            agent_version,
            boot_mem,
            block_size,
            plugged,
        } = message
        else {
            return self.refuse(ctx, said, "something other than hello");
        };

        if protocol != PROTOCOL_VERSION {
            warn!(
                guest = ctx.name(said.guest),
                peer = %said.peer,
                theirs = protocol,
                ours = PROTOCOL_VERSION,
                "the guest speaks another protocol",
            );
            ctx.close(said.guest, said.peer);

            return self.stay();
        }

        if boot_mem == 0 || block_size == 0 {
            warn!(
                guest = ctx.name(said.guest),
                peer = %said.peer,
                boot_mem,
                block_size,
                "the guest declared a geometry that cannot be true",
            );
            ctx.close(said.guest, said.peer);

            return self.stay();
        }

        let ceiling = ctx.ceiling(said.guest);
        let status_interval_ms = ctx.policy.welcome_status_interval.get();

        info!(
            guest = ctx.name(said.guest),
            peer = %said.peer,
            agent_version,
            boot_mem,
            block_size,
            plugged,
            ceiling,
            "welcoming a guest",
        );

        ctx.send(
            said.peer,
            HostMessage::Welcome {
                protocol: PROTOCOL_VERSION,
                ceiling,
                status_interval_ms,
            },
        );

        Phase::Negotiated(Negotiated::new(boot_mem, block_size, said.now))
    }
}
