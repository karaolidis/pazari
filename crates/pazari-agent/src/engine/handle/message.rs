use pazari_proto::HostMessage;
use pazari_types::Timestamp;
use tracing::debug;

use crate::engine::{Engine, effects::Effects, phase::State};

impl Engine {
    pub(in crate::engine) fn on_host(
        &mut self,
        now: Timestamp,
        message: &HostMessage,
        fx: &mut Effects,
    ) {
        if !self.world.connected() {
            return;
        }

        match *message {
            HostMessage::Welcome { .. } => {}
            HostMessage::Ceiling { ceiling } => {
                self.adopt_ceiling(now, ceiling, fx);
            }
            HostMessage::Geometry { hotplugged } => self.world.set_hotplugged(hotplugged),
            HostMessage::Grant { id, target } => {
                debug!(id, target, "host granted the request");

                self.transition(now, fx, |phase, step| phase.on_grant(step, id, target));
            }
            HostMessage::Deny {
                id,
                reason,
                retry_after_ms,
            } => {
                debug!(
                    id,
                    ?reason,
                    retry_after = ?retry_after_ms,
                    "host denied the request",
                );

                self.transition(now, fx, |phase, step| {
                    phase.on_deny(step, id, reason, retry_after_ms)
                });
            }
            HostMessage::ReclaimRequest { id, desired, .. } => {
                debug!(id, desired, "host requested memory back");

                self.transition(now, fx, |phase, step| {
                    phase.on_reclaim_request(step, id, desired)
                });
            }
        }
    }
}
