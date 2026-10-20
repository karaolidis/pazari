use std::{num::NonZeroU64, sync::Arc};

use pazari_proto::GuestMessage;
use pazari_types::Clock;
use tracing::{debug, trace};

use crate::{
    engine::Action,
    runtime::{
        cgroup::{Cgroup, Limit},
        error::RuntimeError,
        reclaim::Reclaimer,
    },
};

/// Executes the engine's actions against the guest.
#[derive(Debug)]
pub struct Applier<C> {
    cgroup: Arc<C>,
    reclaimer: Reclaimer<C>,
}

impl<C: Cgroup + 'static> Applier<C> {
    #[must_use]
    pub fn new(cgroup: Arc<C>, chunk: NonZeroU64, clock: Clock) -> Self {
        Self {
            reclaimer: Reclaimer::new(Arc::clone(&cgroup), chunk, clock),
            cgroup,
        }
    }

    pub const fn reclaimer(&mut self) -> &mut Reclaimer<C> {
        &mut self.reclaimer
    }

    pub async fn apply(
        &mut self,
        actions: Vec<Action>,
        send: &mut impl FnMut(GuestMessage),
    ) -> Result<(), RuntimeError> {
        for action in actions {
            trace!(?action, "applying an action");

            match action {
                Action::SetHigh(bytes) => self.cgroup.set_high(Limit::Bytes(bytes)).await?,
                Action::SetMax(bytes) => self.cgroup.set_max(Limit::Bytes(bytes)).await?,
                Action::SetOomGroup(group) => self.cgroup.set_oom_group(group).await?,
                Action::StartReclaim { bytes, swappiness } => {
                    self.reclaimer.start(bytes, swappiness)?;
                }
                Action::CancelReclaim => {
                    if let Some(reclaim) = self.reclaimer.handle() {
                        debug!("cancelling the in-flight reclaim");
                        reclaim.cancel();
                    }
                }
                Action::Send(message) => send(message),
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use pazari_types::Swappiness;

    use super::*;
    use crate::runtime::testing::{Call, FakeCgroup, MIB, bytes};

    fn applier(cgroup: &Arc<FakeCgroup>) -> Applier<FakeCgroup> {
        Applier::new(Arc::clone(cgroup), bytes(64 * MIB), Clock::start())
    }

    #[tokio::test]
    async fn the_actions_land_in_the_order_the_engine_emitted_them() {
        let cgroup = Arc::new(FakeCgroup::default());
        let mut applier = applier(&cgroup);
        let mut sent = Vec::new();

        applier
            .apply(
                vec![
                    Action::SetMax(2048 * MIB),
                    Action::SetHigh(1792 * MIB),
                    Action::SetOomGroup(true),
                    Action::Send(GuestMessage::Bye {}),
                ],
                &mut |message| sent.push(message),
            )
            .await
            .unwrap();

        assert_eq!(
            cgroup.calls(),
            vec![
                Call::Max(Limit::Bytes(2048 * MIB)),
                Call::High(Limit::Bytes(1792 * MIB)),
                Call::OomGroup(true),
            ]
        );
        assert_eq!(sent, vec![GuestMessage::Bye {}]);
    }

    #[tokio::test]
    async fn a_failed_write_abandons_the_rest_of_the_batch() {
        let cgroup = Arc::new(FakeCgroup::failing(1));
        let mut applier = applier(&cgroup);
        let mut sent = Vec::new();

        let error = applier
            .apply(
                vec![
                    Action::SetHigh(1792 * MIB),
                    Action::SetMax(2048 * MIB),
                    Action::SetOomGroup(true),
                    Action::Send(GuestMessage::Bye {}),
                ],
                &mut |message| sent.push(message),
            )
            .await
            .expect_err("the second write fails");

        assert!(
            error.to_string().contains("memory.max"),
            "unexpected error: {error}"
        );
        assert_eq!(cgroup.calls(), vec![Call::High(Limit::Bytes(1792 * MIB))]);
        assert_eq!(sent, Vec::new());
    }

    #[tokio::test]
    async fn a_reclaim_action_starts_the_release_it_names() {
        let cgroup = Arc::new(FakeCgroup::default());
        let mut applier = applier(&cgroup);
        let mut sent = Vec::new();

        applier
            .apply(
                vec![Action::StartReclaim {
                    bytes: 128 * MIB,
                    swappiness: Some(Swappiness::NONE),
                }],
                &mut |message| sent.push(message),
            )
            .await
            .unwrap();

        assert!(applier.reclaimer().is_running());
        assert_eq!(sent, Vec::new());

        applier.reclaimer().finished().await.unwrap();

        assert_eq!(
            cgroup.calls(),
            vec![
                Call::Reclaim {
                    bytes: 64 * MIB,
                    swappiness: Some(Swappiness::NONE),
                },
                Call::Reclaim {
                    bytes: 64 * MIB,
                    swappiness: Some(Swappiness::NONE),
                },
            ]
        );
    }

    #[tokio::test]
    async fn a_second_reclaim_in_one_batch_is_refused() {
        let cgroup = Arc::new(FakeCgroup::default());
        let mut applier = applier(&cgroup);
        let mut sent = Vec::new();

        let reclaim = Action::StartReclaim {
            bytes: 512 * MIB,
            swappiness: None,
        };

        let error = applier
            .apply(vec![reclaim.clone(), reclaim], &mut |message| {
                sent.push(message);
            })
            .await
            .expect_err("one release at a time");

        assert!(
            error.to_string().contains("is still running"),
            "unexpected error: {error}"
        );
        assert_eq!(sent, Vec::new());
    }

    #[tokio::test]
    async fn a_cancel_stops_the_reclaim_that_is_running() {
        let cgroup = Arc::new(FakeCgroup::default());
        let mut applier = applier(&cgroup);
        let mut sent = Vec::new();

        applier
            .apply(
                vec![Action::StartReclaim {
                    bytes: 1024 * MIB,
                    swappiness: None,
                }],
                &mut |message| sent.push(message),
            )
            .await
            .unwrap();
        applier
            .apply(vec![Action::CancelReclaim], &mut |message| {
                sent.push(message);
            })
            .await
            .unwrap();

        assert!(
            applier
                .reclaimer()
                .handle()
                .expect("a running reclaim")
                .is_cancelled()
        );

        applier.reclaimer().finished().await.unwrap();

        assert_eq!(cgroup.calls(), Vec::new());
        assert_eq!(sent, Vec::new());
    }

    #[tokio::test]
    async fn a_cancel_with_nothing_running_does_nothing() {
        let cgroup = Arc::new(FakeCgroup::default());
        let mut applier = applier(&cgroup);

        applier
            .apply(vec![Action::CancelReclaim], &mut |_| {
                panic!("a cancel sends nothing")
            })
            .await
            .unwrap();

        assert!(!applier.reclaimer().is_running());
        assert_eq!(cgroup.calls(), Vec::new());
    }
}
