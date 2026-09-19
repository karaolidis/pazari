use pazari_proto::DenyReason;

use crate::broker::arbiter::{Arbiter, Ask, Decision, Request};

/// First come, first served under a host floor and a per-guest rate limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Fcfs;

impl Arbiter for Fcfs {
    fn decide(&mut self, request: &Request<'_>) -> Decision {
        match request.ask {
            Ask::Release { target } => Decision::Grant { target },
            Ask::Grow { target, .. } => Self::grow(request, target),
        }
    }
}

impl Fcfs {
    fn grow(request: &Request<'_>, target: u64) -> Decision {
        let policy = request.policy;
        let interval = policy.grant_min_interval.get();

        if let Some(granted) = request.guest.granted {
            let since = request.now.saturating_duration_since(granted);

            if since < interval {
                let reason = DenyReason::RateLimited;
                let retry_after = interval.saturating_sub(since);
                return Decision::Deny {
                    reason,
                    retry_after: Some(retry_after),
                };
            }
        }

        let Some(available) = request.host.available else {
            let reason = DenyReason::HostPressure;
            return Decision::Deny {
                reason,
                retry_after: Some(interval),
            };
        };

        let committed = request.others.iter().fold(0_u64, |total, other| {
            total.saturating_add(other.outstanding())
        });

        let wanted = target.saturating_sub(request.guest.size);

        let projected = available.saturating_sub(committed).saturating_sub(wanted);

        if projected < policy.host_floor {
            let reason = DenyReason::HostPressure;
            return Decision::Deny {
                reason,
                retry_after: Some(interval),
            };
        }

        Decision::Grant { target }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pazari_proto::{GrantReason, Urgency};

    use super::*;
    use crate::broker::testing::{
        CEILING, FLOOR, GIB, MIB, at, guest, offer, policy, request, view,
    };

    fn grow(target: u64) -> Ask {
        Ask::Grow {
            target,
            reason: GrantReason::Throttle,
            urgency: Urgency::High,
        }
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
            Some(8 * GIB),
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
            Some(FLOOR + GIB),
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
            Some(FLOOR + GIB),
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
            Some(FLOOR + GIB),
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
            Some(8 * GIB),
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
            Some(8 * GIB),
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
            None,
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

        let decision = arbiter.decide(&request(at(100), asking, offer(2048 * MIB), &[], None));

        assert_eq!(decision, Decision::Grant { target: 2048 * MIB });
    }
}
