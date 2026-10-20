use pazari_proto::DenyReason;

use crate::broker::{
    arbiter::{Arbiter, Ask, Decision, GuestView, Request},
    policy::Revocation,
    registry::GuestId,
};

/// First come, first served under a host floor and a per-guest rate limit.
///
/// Revocation takes from the guest holding the most over its boot memory;
/// ties go to the guest declared first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Fcfs;

impl Arbiter for Fcfs {
    fn decide(&mut self, request: &Request<'_>) -> Decision {
        match request.ask {
            Ask::Release { target } => Decision::Grant { target },
            Ask::Grow { target } => Self::grow(request, target),
        }
    }

    fn revoke(
        &mut self,
        candidates: &[GuestView<'_>],
        revocation: Revocation,
    ) -> Option<(GuestId, u64)> {
        let mut most: Option<(GuestId, u64)> = None;

        for candidate in candidates {
            let over = candidate.geometry.hotplugged;

            if over > 0 && most.is_none_or(|(_, held)| over > held) {
                most = Some((candidate.id, over));
            }
        }

        most.map(|(guest, over)| (guest, over.min(revocation.step.get())))
    }
}

impl Fcfs {
    fn grow(request: &Request<'_>, target: u64) -> Decision {
        let policy = request.policy;
        let interval = policy.grant_min_interval.get();

        if let Some(granted) = request.guest.granted {
            let since = request.now.saturating_duration_since(granted);

            if since < interval {
                return Decision::Deny {
                    reason: DenyReason::RateLimited,
                    retry_after: Some(interval.saturating_sub(since)),
                };
            }
        }

        let Some(available) = request.host.grantable(policy.grantable) else {
            return Decision::Deny {
                reason: DenyReason::HostPressure,
                retry_after: Some(interval),
            };
        };

        let committed = request.others.iter().fold(0_u64, |total, other| {
            total.saturating_add(other.outstanding())
        });

        let wanted = target.saturating_sub(request.guest.geometry.size);

        let projected = available.saturating_sub(committed).saturating_sub(wanted);

        if projected < policy.grant_floor {
            return Decision::Deny {
                reason: DenyReason::HostPressure,
                retry_after: Some(interval),
            };
        }

        Decision::Grant { target }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::broker::{
        guest::Geometry,
        registry::GuestSpec,
        testing::{
            BOOT, CEILING, FLOOR, GIB, GRANTING, MIB, POLICY, REVOKE_STEP, at, guest, host, offer,
            policy, request, revoking, view,
        },
    };

    fn grow(target: u64) -> Ask {
        Ask::Grow { target }
    }

    #[test]
    fn a_grow_the_host_can_afford_is_granted() {
        let spec = guest("one", CEILING);
        let mut arbiter = Fcfs;

        let decision = arbiter.decide(&request(
            at(100),
            view(&spec, 2048 * MIB, None),
            grow(3072 * MIB),
            &[],
            host(Some(8 * GIB), None),
            &POLICY,
        ));

        assert_eq!(decision, Decision::Grant { target: 3072 * MIB });
    }

    #[test]
    fn a_grow_that_would_eat_the_floor_is_refused() {
        let spec = guest("one", CEILING);
        let mut arbiter = Fcfs;

        let decision = arbiter.decide(&request(
            at(100),
            view(&spec, 2048 * MIB, None),
            grow(4096 * MIB),
            &[],
            host(Some(FLOOR + GIB), None),
            &POLICY,
        ));

        assert_eq!(
            decision,
            Decision::Deny {
                reason: DenyReason::HostPressure,
                retry_after: Some(policy().grant_min_interval.get()),
            }
        );
    }

    #[test]
    fn a_grow_that_lands_exactly_on_the_floor_is_granted() {
        let spec = guest("one", CEILING);
        let mut arbiter = Fcfs;

        let decision = arbiter.decide(&request(
            at(100),
            view(&spec, 2048 * MIB, None),
            grow(3072 * MIB),
            &[],
            host(Some(FLOOR + GIB), None),
            &POLICY,
        ));

        assert_eq!(decision, Decision::Grant { target: 3072 * MIB });
    }

    #[test]
    fn memory_promised_to_another_guest_is_already_spent() {
        let ours = guest("one", CEILING);
        let theirs = guest("two", CEILING);
        let mut arbiter = Fcfs;

        let others = [view(&theirs, 2048 * MIB, Some(3072 * MIB))];

        let decision = arbiter.decide(&request(
            at(100),
            view(&ours, 2048 * MIB, None),
            grow(3072 * MIB),
            &others,
            host(Some(FLOOR + GIB), None),
            &POLICY,
        ));

        assert_eq!(
            decision,
            Decision::Deny {
                reason: DenyReason::HostPressure,
                retry_after: Some(policy().grant_min_interval.get()),
            }
        );
    }

    #[test]
    fn a_guest_granted_a_moment_ago_is_told_to_wait() {
        let spec = guest("one", CEILING);
        let mut arbiter = Fcfs;

        let mut asking = view(&spec, 2048 * MIB, None);
        asking.granted = Some(at(98));

        let decision = arbiter.decide(&request(
            at(100),
            asking,
            grow(3072 * MIB),
            &[],
            host(Some(8 * GIB), None),
            &POLICY,
        ));

        assert_eq!(
            decision,
            Decision::Deny {
                reason: DenyReason::RateLimited,
                retry_after: Some(Duration::from_secs(3)),
            }
        );
    }

    #[test]
    fn a_guest_granted_long_enough_ago_may_ask_again() {
        let spec = guest("one", CEILING);
        let mut arbiter = Fcfs;

        let mut asking = view(&spec, 2048 * MIB, None);
        asking.granted = Some(at(95));

        let decision = arbiter.decide(&request(
            at(100),
            asking,
            grow(3072 * MIB),
            &[],
            host(Some(8 * GIB), None),
            &POLICY,
        ));

        assert_eq!(decision, Decision::Grant { target: 3072 * MIB });
    }

    #[test]
    fn a_host_the_daemon_has_not_read_yet_gives_nothing_away() {
        let spec = guest("one", CEILING);
        let mut arbiter = Fcfs;

        let decision = arbiter.decide(&request(
            at(100),
            view(&spec, 2048 * MIB, None),
            grow(3072 * MIB),
            &[],
            host(None, None),
            &POLICY,
        ));

        assert_eq!(
            decision,
            Decision::Deny {
                reason: DenyReason::HostPressure,
                retry_after: Some(policy().grant_min_interval.get()),
            }
        );
    }

    #[test]
    fn a_grow_only_free_swap_can_cover_is_granted_where_swap_is_granted() {
        let spec = guest("one", CEILING);
        let mut arbiter = Fcfs;

        let asking = view(&spec, 2048 * MIB, None);

        let decision = arbiter.decide(&request(
            at(100),
            asking,
            grow(4096 * MIB),
            &[],
            host(Some(FLOOR + GIB), Some(2 * GIB)),
            &GRANTING,
        ));

        assert_eq!(decision, Decision::Grant { target: 4096 * MIB });

        let decision = arbiter.decide(&request(
            at(100),
            asking,
            grow(4096 * MIB),
            &[],
            host(Some(FLOOR + GIB), None),
            &POLICY,
        ));

        assert_eq!(
            decision,
            Decision::Deny {
                reason: DenyReason::HostPressure,
                retry_after: Some(policy().grant_min_interval.get()),
            },
            "swap was counted for a host that promises only RAM",
        );
    }

    #[test]
    fn a_grow_that_would_eat_the_floor_even_with_the_swap_counted_is_refused() {
        let spec = guest("one", CEILING);
        let mut arbiter = Fcfs;

        let decision = arbiter.decide(&request(
            at(100),
            view(&spec, 2048 * MIB, None),
            grow(4096 * MIB),
            &[],
            host(Some(FLOOR + GIB), Some(512 * MIB)),
            &GRANTING,
        ));

        assert_eq!(
            decision,
            Decision::Deny {
                reason: DenyReason::HostPressure,
                retry_after: Some(policy().grant_min_interval.get()),
            }
        );
    }

    #[test]
    fn a_host_granting_swap_it_has_not_read_gives_nothing_away() {
        let spec = guest("one", CEILING);
        let mut arbiter = Fcfs;

        let decision = arbiter.decide(&request(
            at(100),
            view(&spec, 2048 * MIB, None),
            grow(3072 * MIB),
            &[],
            host(Some(8 * GIB), None),
            &GRANTING,
        ));

        assert_eq!(
            decision,
            Decision::Deny {
                reason: DenyReason::HostPressure,
                retry_after: Some(policy().grant_min_interval.get()),
            }
        );
    }

    #[test]
    fn a_release_is_taken_whatever_the_host_looks_like() {
        let spec = guest("one", CEILING);
        let mut arbiter = Fcfs;

        let mut asking = view(&spec, 3072 * MIB, None);
        asking.granted = Some(at(100));

        let decision = arbiter.decide(&request(
            at(100),
            asking,
            offer(2048 * MIB),
            &[],
            host(None, None),
            &POLICY,
        ));

        assert_eq!(decision, Decision::Grant { target: 2048 * MIB });
    }

    fn revocation() -> Revocation {
        revoking().revocation.expect("the fixture revokes")
    }

    fn candidate(spec: &GuestSpec, index: usize, size: u64) -> GuestView<'_> {
        GuestView {
            id: GuestId::from_index(index),
            geometry: Geometry {
                hotplugged: size.saturating_sub(BOOT),
                ..view(spec, size, None).geometry
            },
            ..view(spec, size, None)
        }
    }

    #[test]
    fn revocation_takes_from_the_guest_holding_the_most_hot_plugged() {
        let spec = guest("one", CEILING);
        let candidates = [
            candidate(&spec, 0, BOOT + 64 * MIB),
            candidate(&spec, 1, BOOT + 128 * MIB),
        ];

        assert_eq!(
            Fcfs.revoke(&candidates, revocation()),
            Some((GuestId::from_index(1), 128 * MIB))
        );
    }

    #[test]
    fn revocation_asks_for_at_most_one_step() {
        let spec = guest("one", CEILING);
        let candidates = [candidate(&spec, 0, BOOT + GIB)];

        assert_eq!(
            Fcfs.revoke(&candidates, revocation()),
            Some((GuestId::from_index(0), REVOKE_STEP))
        );
    }

    #[test]
    fn revocation_ties_go_to_the_guest_declared_first() {
        let spec = guest("one", CEILING);
        let candidates = [
            candidate(&spec, 0, BOOT + 128 * MIB),
            candidate(&spec, 1, BOOT + 128 * MIB),
        ];

        assert_eq!(
            Fcfs.revoke(&candidates, revocation()),
            Some((GuestId::from_index(0), 128 * MIB))
        );
    }

    #[test]
    fn guests_at_their_boot_memory_are_left_alone() {
        let spec = guest("one", CEILING);

        assert_eq!(
            Fcfs.revoke(&[candidate(&spec, 0, BOOT)], revocation()),
            None
        );
        assert_eq!(Fcfs.revoke(&[], revocation()), None);
    }
}
