use pazari_metrics::{
    Counter, Family, Fraction, Gauge, ModeLabel, ReasonLabel, Registry, Saturating, Unit,
};
use pazari_proto::{GuestMessage, HostMessage};

use crate::engine::{Action, Engine, Event, Mode, Snapshot};

const PREFIX: &str = "pazari_agent";

#[derive(Debug, Default)]
pub struct Observer(Option<Series>);

impl Observer {
    #[must_use]
    pub fn new() -> (Self, Registry) {
        let series = Series::default();
        let mut registry = Registry::with_prefix(PREFIX);

        series.register(&mut registry);

        (Self(Some(series)), registry)
    }

    pub fn observe_event(&self, event: &Event) {
        let Some(series) = &self.0 else {
            return;
        };

        match event {
            Event::Tick { snapshot, .. } => series.reading.observe(*snapshot),
            Event::ThrottleWake { .. } => {
                series.throttle_wakes.inc();
            }
            Event::Connected { .. } => {
                series.connections.inc();
            }
            Event::Disconnected { .. } => {
                series.disconnections.inc();
            }
            Event::Host {
                message: HostMessage::Grant { .. },
                ..
            } => {
                series.grants.inc();
            }
            Event::Host {
                message: HostMessage::Deny { reason, .. },
                ..
            } => {
                series
                    .denies
                    .get_or_create(&ReasonLabel {
                        reason: reason.as_str(),
                    })
                    .inc();
            }
            _ => {}
        }
    }

    pub fn observe_actions(&self, actions: &[Action]) {
        let Some(series) = &self.0 else {
            return;
        };

        for action in actions {
            match action {
                Action::Send(GuestMessage::GrantRequest { .. }) => {
                    series.grant_requests.inc();
                }
                Action::Send(GuestMessage::OfferRelease { .. }) => {
                    series.offers.inc();
                }
                Action::StartReclaim { bytes, .. } => {
                    series.reclaims.inc();
                    series.reclaim_asked.inc_by(*bytes);
                }
                _ => {}
            }
        }
    }

    pub fn refresh(&mut self, engine: &Engine) {
        let Some(series) = &mut self.0 else {
            return;
        };

        let mode = engine.mode();

        for candidate in Mode::ALL {
            series
                .mode
                .get_or_create(&ModeLabel {
                    mode: candidate.as_str(),
                })
                .set(i64::from(candidate == mode));
        }

        if mode == Mode::Exhausted && series.was != Some(Mode::Exhausted) {
            series.exhaustions.inc();
        }

        series.was = Some(mode);

        series.limits.high.set_saturating(engine.high());
        series.limits.max.set_saturating(engine.max());
        series.limits.plugged.set_saturating(engine.plugged());
        series
            .limits
            .ceiling
            .set_saturating(engine.ceiling().unwrap_or(0));
        series.limits.band_open.set(i64::from(engine.band_open()));
        series.connected.set(i64::from(engine.is_connected()));
    }
}

#[derive(Debug, Default)]
struct Series {
    limits: Limits,
    reading: Reading,

    mode: Family<ModeLabel, Gauge>,
    connected: Gauge,

    throttle_wakes: Counter,
    grant_requests: Counter,
    grants: Counter,
    denies: Family<ReasonLabel, Counter>,
    offers: Counter,
    reclaims: Counter,
    reclaim_asked: Counter,
    exhaustions: Counter,

    connections: Counter,
    disconnections: Counter,

    was: Option<Mode>,
}

impl Series {
    fn register(&self, registry: &mut Registry) {
        self.limits.register(registry);
        self.reading.register(registry);

        registry.register("mode", "Which state the agent is in", self.mode.clone());
        registry.register(
            "connected",
            "Whether the agent is connected to the host and past the handshake",
            self.connected.clone(),
        );

        registry.register(
            "throttle_wakes",
            "Times the workload's throttle counters woke the agent",
            self.throttle_wakes.clone(),
        );
        registry.register(
            "grant_requests",
            "Requests for more memory sent to the host",
            self.grant_requests.clone(),
        );
        registry.register(
            "grants",
            "Grants the host answered with",
            self.grants.clone(),
        );
        registry.register(
            "denies",
            "Refusals the host answered with, by the reason given",
            self.denies.clone(),
        );
        registry.register(
            "offers",
            "Offers to release memory sent to the host",
            self.offers.clone(),
        );
        registry.register(
            "reclaims",
            "Reclaims started on the workload cgroup",
            self.reclaims.clone(),
        );
        registry.register_with_unit(
            "reclaim_asked",
            "Memory those reclaims asked the kernel for",
            Unit::Bytes,
            self.reclaim_asked.clone(),
        );
        registry.register(
            "exhaustions",
            "Times the workload wanted more than the ceiling allows",
            self.exhaustions.clone(),
        );

        registry.register(
            "connections",
            "Connections to the host that reached the handshake",
            self.connections.clone(),
        );
        registry.register(
            "disconnections",
            "Connections to the host that were lost",
            self.disconnections.clone(),
        );
    }
}

#[derive(Debug, Default)]
struct Limits {
    high: Gauge,
    max: Gauge,
    plugged: Gauge,
    ceiling: Gauge,
    band_open: Gauge,
}

impl Limits {
    fn register(&self, registry: &mut Registry) {
        registry.register_with_unit(
            "memory_high",
            "The workload cgroup's memory.high",
            Unit::Bytes,
            self.high.clone(),
        );
        registry.register_with_unit(
            "memory_max",
            "The workload cgroup's memory.max",
            Unit::Bytes,
            self.max.clone(),
        );
        registry.register_with_unit(
            "plugged",
            "What the guest's memory device holds",
            Unit::Bytes,
            self.plugged.clone(),
        );
        registry.register_with_unit(
            "ceiling",
            "The most memory the host allows, 0 until it has said",
            Unit::Bytes,
            self.ceiling.clone(),
        );
        registry.register(
            "band_open",
            "Whether the brake is off and the workload may burst",
            self.band_open.clone(),
        );
    }
}

#[derive(Debug, Default)]
struct Reading {
    workload_usage: Gauge,
    swap_used: Gauge,
    high_events: Gauge,
    max_events: Gauge,
    psi_some10: Fraction,
}

impl Reading {
    fn observe(&self, snapshot: Snapshot) {
        self.workload_usage.set_saturating(snapshot.workload_usage);
        self.swap_used.set_saturating(snapshot.swap_used);
        self.high_events.set_saturating(snapshot.high_events);
        self.max_events.set_saturating(snapshot.max_events);
        self.psi_some10.set(snapshot.psi_some10.into());
    }

    fn register(&self, registry: &mut Registry) {
        registry.register_with_unit(
            "workload_usage",
            "What the workload cgroup was holding",
            Unit::Bytes,
            self.workload_usage.clone(),
        );
        registry.register_with_unit(
            "swap_used",
            "What the guest had swapped out",
            Unit::Bytes,
            self.swap_used.clone(),
        );
        registry.register(
            "high_events",
            "The workload cgroup's memory.events high, as the kernel counts it",
            self.high_events.clone(),
        );
        registry.register(
            "max_events",
            "The workload cgroup's memory.events max, as the kernel counts it",
            self.max_events.clone(),
        );
        registry.register(
            "psi_some10",
            "The workload's memory some avg10, in percent of wall time",
            self.psi_some10.clone(),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pazari_metrics::Encoded;
    use pazari_proto::DenyReason;
    use pazari_types::Swappiness;

    use super::*;
    use crate::engine::testing::*;

    fn carries(registry: &Registry, series: &str) {
        let text = registry.encoded().expect("a registry encodes");

        assert!(text.contains(series), "{series} is not in:\n{text}");
    }

    fn connected() -> Event {
        Event::Connected {
            now: at(0),
            ceiling: CEILING,
            status_interval: HOUR,
        }
    }

    fn answered(message: HostMessage) -> Event {
        Event::Host {
            now: at(1),
            message,
        }
    }

    #[test]
    fn what_the_engine_holds_the_workload_to_is_published() {
        let (mut observer, registry) = Observer::new();
        let mut engine = started();

        observer.refresh(&engine);

        carries(&registry, "pazari_agent_mode{mode=\"steady\"} 1");
        carries(&registry, "pazari_agent_mode{mode=\"titrating\"} 0");
        carries(
            &registry,
            &format!("pazari_agent_memory_high_bytes {HIGH0}"),
        );
        carries(&registry, &format!("pazari_agent_memory_max_bytes {MAX0}"));
        carries(&registry, &format!("pazari_agent_plugged_bytes {BOOT}"));
        carries(&registry, &format!("pazari_agent_ceiling_bytes {CEILING}"));
        carries(&registry, "pazari_agent_band_open 0");
        carries(&registry, "pazari_agent_connected 1");

        engine.handle(Event::Disconnected { now: at(200) });
        observer.refresh(&engine);

        carries(&registry, "pazari_agent_connected 0");
    }

    #[test]
    fn a_ceiling_the_host_has_not_named_reads_as_nothing() {
        let (mut observer, registry) = Observer::new();
        let (engine, _) = Engine::try_new(config(), boot()).expect("a workable boot");

        observer.refresh(&engine);

        carries(&registry, "pazari_agent_ceiling_bytes 0");
        carries(&registry, "pazari_agent_mode{mode=\"titrating\"} 1");
        carries(&registry, "pazari_agent_connected 0");
    }

    #[test]
    fn a_tick_publishes_the_reading_it_carried() {
        let (observer, registry) = Observer::new();

        observer.observe_event(&Event::Tick {
            now: at(1),
            snapshot: Snapshot {
                swap_used: 128 * MIB,
                high_events: 9,
                max_events: 3,
                psi_some10: 12.5,
                ..snap(BOOT, BUSY, 9)
            },
        });

        carries(
            &registry,
            &format!("pazari_agent_workload_usage_bytes {BUSY}"),
        );
        carries(
            &registry,
            &format!("pazari_agent_swap_used_bytes {}", 128 * MIB),
        );
        carries(&registry, "pazari_agent_high_events 9");
        carries(&registry, "pazari_agent_max_events 3");
        carries(&registry, "pazari_agent_psi_some10 12.5");
    }

    #[test]
    fn what_the_agent_hears_from_the_host_is_counted() {
        let (observer, registry) = Observer::new();

        observer.observe_event(&connected());
        observer.observe_event(&answered(HostMessage::Grant {
            id: 1,
            target: 2560 * MIB,
        }));
        observer.observe_event(&answered(HostMessage::Deny {
            id: 2,
            reason: DenyReason::HostPressure,
            retry_after_ms: Some(Duration::from_secs(5)),
        }));
        observer.observe_event(&answered(HostMessage::Deny {
            id: 3,
            reason: DenyReason::Ceiling,
            retry_after_ms: None,
        }));
        observer.observe_event(&Event::ThrottleWake {
            now: at(2),
            high_events: 1,
            max_events: 0,
        });

        carries(&registry, "pazari_agent_grants_total 1");
        carries(
            &registry,
            "pazari_agent_denies_total{reason=\"host_pressure\"} 1",
        );
        carries(&registry, "pazari_agent_denies_total{reason=\"ceiling\"} 1");
        carries(&registry, "pazari_agent_throttle_wakes_total 1");
        carries(&registry, "pazari_agent_connections_total 1");
    }

    #[test]
    fn sessions_with_the_host_are_counted_as_they_come_and_go() {
        let (observer, registry) = Observer::new();

        observer.observe_event(&connected());
        observer.observe_event(&Event::Disconnected { now: at(1) });
        observer.observe_event(&connected());
        observer.observe_event(&connected());

        carries(&registry, "pazari_agent_connections_total 3");
        carries(&registry, "pazari_agent_disconnections_total 1");
    }

    #[test]
    fn what_the_agent_asks_for_is_counted_as_it_goes_out() {
        let (observer, registry) = Observer::new();

        observer.observe_actions(&[
            Action::SetHigh(HIGH0),
            request(1, BOOT, 2560 * MIB),
            Action::StartReclaim {
                bytes: 256 * MIB,
                swappiness: Some(Swappiness::NONE),
            },
        ]);
        observer.observe_actions(&[Action::Send(GuestMessage::OfferRelease {
            id: 2,
            current: BOOT,
            releasable: BURST,
        })]);

        carries(&registry, "pazari_agent_grant_requests_total 1");
        carries(&registry, "pazari_agent_offers_total 1");
        carries(&registry, "pazari_agent_reclaims_total 1");
        carries(
            &registry,
            &format!("pazari_agent_reclaim_asked_bytes_total {}", 256 * MIB),
        );
    }

    #[test]
    fn falling_into_exhaustion_is_counted_once_per_fall() {
        let (mut observer, registry) = Observer::new();
        let mut engine = titrated(config(), 2560 * MIB);

        observer.refresh(&engine);
        wake(&mut engine, at(100), 1);
        observer.refresh(&engine);

        carries(&registry, "pazari_agent_exhaustions_total 0");

        host(
            &mut engine,
            at(101),
            HostMessage::Deny {
                id: 1,
                reason: DenyReason::Ceiling,
                retry_after_ms: None,
            },
        );
        observer.refresh(&engine);

        carries(&registry, "pazari_agent_mode{mode=\"exhausted\"} 1");
        carries(&registry, "pazari_agent_mode{mode=\"awaiting_grant\"} 0");
        carries(&registry, "pazari_agent_exhaustions_total 1");

        observer.refresh(&engine);

        carries(&registry, "pazari_agent_exhaustions_total 1");
    }

    #[test]
    fn an_agent_the_operator_asked_nothing_of_keeps_no_series() {
        let mut observer = Observer::default();
        let engine = started();

        observer.observe_event(&connected());
        observer.observe_actions(&[Action::SetHigh(HIGH0)]);
        observer.refresh(&engine);
    }
}
