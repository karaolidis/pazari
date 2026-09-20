use pazari_types::MemInfo;
use tracing::{info, warn};

use crate::{
    broker::{Policy, Registry},
    run::error::RunError,
    runtime::{
        HostMemory,
        memory::{PRESSURE, SWAPS},
    },
};

const ZRAM: &str = "/dev/zram";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Demand {
    /// Every ceiling plus the memory the host keeps for itself.
    pub asked: u64,

    /// `MemTotal + SwapTotal`.
    pub have: u64,
}

impl Demand {
    #[must_use]
    pub fn of(policy: &Policy, registry: &Registry, info: &MemInfo) -> Self {
        Self {
            asked: registry.iter().fold(policy.host_floor, |total, (_, spec)| {
                total.saturating_add(spec.ceiling)
            }),
            have: info.total.saturating_add(info.swap_total),
        }
    }

    #[must_use]
    pub const fn crowded(self) -> bool {
        self.asked > self.have
    }
}

pub async fn survey<M: HostMemory>(
    memory: &M,
    policy: &Policy,
    registry: &Registry,
) -> Result<(), RunError> {
    let demand = Demand::of(policy, registry, &memory.meminfo().await?);
    let Demand { asked, have } = demand;

    if demand.crowded() {
        warn!(
            asked,
            have,
            "the guests at their ceilings and the host floor come to more than \
             this host holds in RAM and swap together",
        );
    } else {
        info!(
            asked,
            have, "the host's RAM and swap against what the guests may hold"
        );
    }

    if policy.revocation.is_some() && memory.pressure().await?.is_none() {
        return Err(RunError::Pressureless { path: PRESSURE });
    }

    if policy.grant_swap {
        zram(memory).await;
    }

    Ok(())
}

async fn zram<M: HostMemory>(memory: &M) {
    let swaps = match memory.swaps().await {
        Ok(swaps) => swaps,
        Err(error) => {
            warn!(
                %error,
                path = SWAPS,
                "failed to look for zram swap, whose capacity would be overstated",
            );

            return;
        }
    };

    let devices = swaps
        .into_iter()
        .filter(|device| device.starts_with(ZRAM))
        .collect::<Vec<_>>();

    if !devices.is_empty() {
        let devices = devices.join(", ");

        warn!(
            %devices,
            "swap is on zram, whose SwapFree counts what it would hold \
             uncompressed, so a grant backed by it may not fit",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        broker::{
            GuestSpec,
            testing::{CEILING, FLOOR, GIB, granting, policy, registry, revoking, vsock},
        },
        runtime::testing::{MockMemory, meminfo},
    };

    const ASKED: u64 = 2 * CEILING + FLOOR;

    fn host(total: u64, swap_total: u64) -> MockMemory {
        MockMemory::new(meminfo(total, total, swap_total)).pressured(1.0)
    }

    fn unmeasured(total: u64, swap_total: u64) -> MockMemory {
        MockMemory::new(meminfo(total, total, swap_total))
    }

    fn guests(ceilings: &[u64]) -> Registry {
        let mut registry = Registry::new();

        for (index, ceiling) in ceilings.iter().enumerate() {
            let cid = 3 + u32::try_from(index).expect("a handful of guests");

            registry
                .insert(
                    vsock(cid),
                    GuestSpec {
                        name: format!("guest{index}"),
                        ceiling: *ceiling,
                    },
                )
                .expect("a guest of its own");
        }

        registry
    }

    fn demand(policy: &Policy, registry: &Registry, total: u64, swap_total: u64) -> Demand {
        Demand::of(policy, registry, &meminfo(total, total, swap_total))
    }

    #[test]
    fn a_host_that_holds_every_ceiling_in_ram_is_nothing_to_say_anything_about() {
        let demand = demand(&policy(), &registry(), ASKED, 0);

        assert_eq!(
            demand,
            Demand {
                asked: ASKED,
                have: ASKED,
            }
        );
        assert!(!demand.crowded());
    }

    #[test]
    fn two_guests_that_may_each_burst_to_the_whole_host_are_an_ordinary_deployment() {
        let policy = Policy {
            host_floor: 0,
            ..policy()
        };

        let demand = demand(&policy, &guests(&[32 * GIB, 32 * GIB]), 32 * GIB, 64 * GIB);

        assert!(
            !demand.crowded(),
            "32G of RAM and 64G of swap was called crowded by two 32G guests",
        );
    }

    #[test]
    fn a_host_whose_ram_and_swap_together_fall_short_is_crowded() {
        let demand = demand(&policy(), &guests(&[32 * GIB, 32 * GIB]), 4 * GIB, 4 * GIB);

        assert!(demand.crowded());
    }

    #[test]
    fn swap_is_what_carries_a_host_over_the_line() {
        assert!(demand(&policy(), &registry(), ASKED - GIB, 0).crowded());
        assert!(!demand(&policy(), &registry(), ASKED - GIB, GIB).crowded());
    }

    #[test]
    fn the_host_floor_is_asked_for_even_where_no_guest_is() {
        let demand = demand(&policy(), &Registry::new(), FLOOR - 1, 0);

        assert_eq!(demand.asked, FLOOR);
        assert!(demand.crowded());
    }

    #[test]
    fn a_ceiling_that_overruns_the_arithmetic_is_crowded_rather_than_wrapped() {
        let demand = demand(&policy(), &guests(&[u64::MAX]), 8 * GIB, 8 * GIB);

        assert_eq!(demand.asked, u64::MAX);
        assert!(demand.crowded());
    }

    #[tokio::test]
    async fn a_crowded_host_still_starts() {
        let memory = host(4 * GIB, 4 * GIB);

        assert!(
            survey(&memory, &policy(), &guests(&[32 * GIB, 32 * GIB]))
                .await
                .is_ok(),
            "a warning gated the daemon",
        );
    }

    #[tokio::test]
    async fn a_host_asked_to_revoke_without_pressure_accounting_is_refused() {
        let memory = unmeasured(ASKED, 0);

        assert_eq!(
            survey(&memory, &revoking(), &registry())
                .await
                .expect_err("the safeguard could never fire")
                .to_string(),
            "the daemon was asked to take memory back under pressure, and this host \
             carries no /proc/pressure/memory to measure it with"
        );
    }

    #[tokio::test]
    async fn a_host_that_was_never_asked_to_revoke_needs_no_pressure_accounting() {
        let memory = unmeasured(ASKED, 0);

        assert!(survey(&memory, &policy(), &registry()).await.is_ok());
        assert!(survey(&memory, &granting(), &registry()).await.is_ok());
    }

    #[tokio::test]
    async fn a_host_on_zram_starts_whether_its_swap_is_granted_or_not() {
        let memory = host(ASKED, GIB).swapping(&["/dev/zram0", "/swap/swapfile"]);

        assert!(survey(&memory, &policy(), &registry()).await.is_ok());
        assert!(survey(&memory, &granting(), &registry()).await.is_ok());
    }

    #[tokio::test]
    async fn a_swap_list_that_cannot_be_read_costs_only_the_zram_warning() {
        let memory = host(ASKED, GIB).unreadable_swaps();

        assert!(survey(&memory, &granting(), &registry()).await.is_ok());
    }
}
