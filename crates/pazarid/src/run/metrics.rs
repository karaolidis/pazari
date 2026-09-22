use pazari_metrics::{
    Counter, Family, Fraction, Gauge, GuestLabel, KindLabel, ReasonLabel, Registry, Saturating,
    Unit,
};
use pazari_proto::HostMessage;

use crate::broker::{Action, Broker, Event};

const PREFIX: &str = "pazarid";

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

    #[must_use]
    pub fn device(&self, guest: &str) -> DeviceMetrics {
        let label = GuestLabel {
            guest: guest.to_owned(),
        };

        DeviceMetrics(self.0.as_ref().map(|series| Device {
            reconnects: series.devices.reconnects.get_or_create_owned(&label),
            seed_failures: series.devices.seed_failures.get_or_create_owned(&label),
            refusals: series.devices.refusals.get_or_create_owned(&label),
        }))
    }

    pub fn observe_event(&self, event: &Event) {
        let Some(series) = &self.0 else {
            return;
        };

        match event {
            Event::Connected { .. } => {
                series.connections.inc();
            }
            Event::Disconnected { .. } => {
                series.disconnections.inc();
            }
            Event::Message { message, .. } => {
                series
                    .messages
                    .get_or_create(&KindLabel {
                        kind: message.kind(),
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
                Action::Send {
                    message: HostMessage::Grant { .. },
                    ..
                } => {
                    series.grants.inc();
                }
                Action::Send {
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
                Action::Send {
                    message: HostMessage::ReclaimRequest { .. },
                    ..
                } => {
                    series.reclaim_requests.inc();
                }
                Action::Close { .. } => {
                    series.closes.inc();
                }
                _ => {}
            }
        }
    }

    pub fn refresh(&self, broker: &Broker) {
        let Some(series) = &self.0 else {
            return;
        };

        let guests = broker.guests();

        for (id, entry) in guests.iter() {
            let view = entry.view(id);
            let label = GuestLabel {
                guest: view.spec.name.clone(),
            };
            let reclaiming = guests
                .negotiated(id)
                .is_some_and(|session| session.reclaim.is_some());

            series
                .guests
                .size
                .get_or_create(&label)
                .set_saturating(view.size);
            series
                .guests
                .ceiling
                .get_or_create(&label)
                .set_saturating(view.spec.ceiling.get());
            series
                .guests
                .connected
                .get_or_create(&label)
                .set(i64::from(view.connected));
            series
                .guests
                .stale
                .get_or_create(&label)
                .set(i64::from(view.stale));
            series
                .guests
                .reclaiming
                .get_or_create(&label)
                .set(i64::from(reclaiming));
        }

        let host = broker.host();

        series
            .host
            .available
            .set_saturating(host.available.unwrap_or(0));
        series
            .host
            .swap_free
            .set_saturating(host.swap_free.unwrap_or(0));
        series.host.psi.set(host.psi.unwrap_or(0.0).into());
    }
}

#[derive(Debug, Clone, Default)]
pub struct DeviceMetrics(Option<Device>);

impl DeviceMetrics {
    pub fn reconnected(&self) {
        if let Some(device) = &self.0 {
            device.reconnects.inc();
        }
    }

    pub fn seed_failed(&self) {
        if let Some(device) = &self.0 {
            device.seed_failures.inc();
        }
    }

    pub fn refused(&self) {
        if let Some(device) = &self.0 {
            device.refusals.inc();
        }
    }
}

#[derive(Debug, Clone)]
struct Device {
    reconnects: Counter,
    seed_failures: Counter,
    refusals: Counter,
}

#[derive(Debug, Default)]
struct Series {
    guests: Guests,
    host: Host,
    devices: Devices,

    grants: Counter,
    denies: Family<ReasonLabel, Counter>,
    reclaim_requests: Counter,
    closes: Counter,

    connections: Counter,
    disconnections: Counter,
    messages: Family<KindLabel, Counter>,
}

impl Series {
    fn register(&self, registry: &mut Registry) {
        self.guests.register(registry);
        self.host.register(registry);
        self.devices.register(registry);

        registry.register("grants", "Grants sent to guests", self.grants.clone());
        registry.register(
            "denies",
            "Requests refused, by the reason given",
            self.denies.clone(),
        );
        registry.register(
            "reclaim_requests",
            "Reclaim requests sent to guests",
            self.reclaim_requests.clone(),
        );
        registry.register(
            "closes",
            "Connections the broker hung up on",
            self.closes.clone(),
        );

        registry.register(
            "connections",
            "Connections taken on",
            self.connections.clone(),
        );
        registry.register(
            "disconnections",
            "Connections lost",
            self.disconnections.clone(),
        );
        registry.register(
            "guest_messages",
            "Messages received from guests, by kind",
            self.messages.clone(),
        );
    }
}

#[derive(Debug, Default)]
struct Guests {
    size: Family<GuestLabel, Gauge>,
    ceiling: Family<GuestLabel, Gauge>,
    connected: Family<GuestLabel, Gauge>,
    stale: Family<GuestLabel, Gauge>,
    reclaiming: Family<GuestLabel, Gauge>,
}

impl Guests {
    fn register(&self, registry: &mut Registry) {
        registry.register_with_unit(
            "guest_size",
            "What the guest's memory device holds, 0 until it has reported",
            Unit::Bytes,
            self.size.clone(),
        );
        registry.register_with_unit(
            "guest_ceiling",
            "The most memory the guest may hold",
            Unit::Bytes,
            self.ceiling.clone(),
        );
        registry.register(
            "guest_connected",
            "Whether the guest is connected and past the handshake",
            self.connected.clone(),
        );
        registry.register(
            "guest_stale",
            "Whether the guest has stopped sending status",
            self.stale.clone(),
        );
        registry.register(
            "guest_reclaim_outstanding",
            "Whether a reclaim request is waiting on the guest",
            self.reclaiming.clone(),
        );
    }
}

#[derive(Debug, Default)]
struct Host {
    available: Gauge,
    swap_free: Gauge,
    psi: Fraction,
}

impl Host {
    fn register(&self, registry: &mut Registry) {
        registry.register_with_unit(
            "host_available",
            "The host's MemAvailable as the daemon last read it, 0 until it has",
            Unit::Bytes,
            self.available.clone(),
        );
        registry.register_with_unit(
            "host_swap_free",
            "The host's SwapFree as the daemon last read it, 0 until it has",
            Unit::Bytes,
            self.swap_free.clone(),
        );
        registry.register(
            "host_psi",
            "The host's memory some avg10, in percent of wall time, 0 until the daemon has read it",
            self.psi.clone(),
        );
    }
}

#[derive(Debug, Default)]
struct Devices {
    reconnects: Family<GuestLabel, Counter>,
    seed_failures: Family<GuestLabel, Counter>,
    refusals: Family<GuestLabel, Counter>,
}

impl Devices {
    fn register(&self, registry: &mut Registry) {
        registry.register(
            "device_reconnects",
            "Times a guest's memory device was opened again",
            self.reconnects.clone(),
        );
        registry.register(
            "device_seed_failures",
            "Times a guest's memory device would not say what size it is",
            self.seed_failures.clone(),
        );
        registry.register(
            "device_refused_actuations",
            "Resizes a guest's memory device refused",
            self.refusals.clone(),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pazari_metrics::Encoded;
    use pazari_proto::DenyReason;

    use super::*;
    use crate::broker::{
        Broker,
        testing::{
            BOOT, CEILING, GIB, HOST, MIB, ONE, TWO, at, closed, deny, grant, hello, id, pressured,
            report, requested, tick, vsock, welcomed,
        },
    };

    fn scrape(registry: &Registry) -> String {
        registry.encoded().expect("a registry encodes")
    }

    fn carries(registry: &Registry, series: &str) {
        let text = scrape(registry);

        assert!(text.contains(series), "{series} is not in:\n{text}");
    }

    fn reclaim_request(cid: u32) -> Action {
        Action::Send {
            peer: vsock(cid),
            message: HostMessage::ReclaimRequest {
                id: 1,
                desired: 256 * MIB,
                deadline_ms: Duration::from_secs(30),
            },
        }
    }

    fn watched() -> (Observer, Registry, Broker) {
        let (observer, registry) = Observer::new();

        (observer, registry, welcomed(ONE))
    }

    #[test]
    fn what_the_broker_answers_is_counted_as_it_goes_out() {
        let (observer, registry) = Observer::new();

        observer.observe_actions(&[
            grant(ONE, 1, 3072 * MIB),
            deny(ONE, 2, DenyReason::Ceiling),
            requested(ONE, 3072 * MIB),
        ]);
        observer.observe_actions(&[
            deny(TWO, 1, DenyReason::HostPressure),
            reclaim_request(TWO),
            closed(TWO),
        ]);

        carries(&registry, "pazarid_grants_total 1");
        carries(&registry, "pazarid_denies_total{reason=\"ceiling\"} 1");
        carries(
            &registry,
            "pazarid_denies_total{reason=\"host_pressure\"} 1",
        );
        carries(&registry, "pazarid_reclaim_requests_total 1");
        carries(&registry, "pazarid_closes_total 1");
    }

    #[test]
    fn what_a_guest_says_is_counted_by_kind() {
        let (observer, registry) = Observer::new();

        observer.observe_event(&Event::Connected {
            now: at(0),
            peer: vsock(ONE),
        });
        observer.observe_event(&Event::Message {
            now: at(1),
            peer: vsock(ONE),
            message: hello(),
        });
        observer.observe_event(&Event::Message {
            now: at(2),
            peer: vsock(ONE),
            message: report(BOOT, GIB),
        });
        observer.observe_event(&Event::Disconnected {
            now: at(3),
            peer: vsock(ONE),
        });
        observer.observe_event(&Event::Tick { now: at(4) });

        carries(&registry, "pazarid_connections_total 1");
        carries(&registry, "pazarid_disconnections_total 1");
        carries(&registry, "pazarid_guest_messages_total{kind=\"hello\"} 1");
        carries(&registry, "pazarid_guest_messages_total{kind=\"status\"} 1");
    }

    #[test]
    fn a_tick_publishes_every_guest_and_the_host_it_shares() {
        let (observer, registry, mut broker) = watched();

        let _ = broker.handle(pressured(at(1), HOST, GIB, 12.5));

        observer.refresh(&broker);

        carries(
            &registry,
            &format!("pazarid_guest_size_bytes{{guest=\"one\"}} {BOOT}"),
        );
        carries(
            &registry,
            &format!("pazarid_guest_ceiling_bytes{{guest=\"two\"}} {CEILING}"),
        );
        carries(&registry, "pazarid_guest_connected{guest=\"one\"} 1");
        carries(&registry, "pazarid_guest_connected{guest=\"two\"} 0");
        carries(&registry, "pazarid_guest_stale{guest=\"one\"} 0");
        carries(&registry, &format!("pazarid_host_available_bytes {HOST}"));
        carries(&registry, &format!("pazarid_host_swap_free_bytes {GIB}"));
        carries(&registry, "pazarid_host_psi 12.5");
    }

    #[test]
    fn a_guest_that_goes_quiet_or_is_asked_for_memory_says_so() {
        let (observer, registry, mut broker) = watched();

        let _ = broker.handle(Event::Reclaim {
            now: at(1),
            guest: id(ONE),
            desired: 256 * MIB,
        });
        tick(&mut broker, at(4));

        observer.refresh(&broker);

        carries(&registry, "pazarid_guest_stale{guest=\"one\"} 1");
        carries(
            &registry,
            "pazarid_guest_reclaim_outstanding{guest=\"one\"} 1",
        );
        carries(
            &registry,
            "pazarid_guest_reclaim_outstanding{guest=\"two\"} 0",
        );
    }

    #[test]
    fn a_device_that_will_not_answer_is_counted_against_its_guest() {
        let (observer, registry) = Observer::new();
        let device = observer.device("one");

        device.reconnected();
        device.reconnected();
        device.seed_failed();
        device.refused();

        observer.device("two").reconnected();

        carries(
            &registry,
            "pazarid_device_reconnects_total{guest=\"one\"} 2",
        );
        carries(
            &registry,
            "pazarid_device_seed_failures_total{guest=\"one\"} 1",
        );
        carries(
            &registry,
            "pazarid_device_refused_actuations_total{guest=\"one\"} 1",
        );
        carries(
            &registry,
            "pazarid_device_reconnects_total{guest=\"two\"} 1",
        );
    }

    #[test]
    fn a_daemon_the_operator_asked_nothing_of_keeps_no_series() {
        let observer = Observer::default();
        let broker = welcomed(ONE);

        observer.observe_event(&Event::Connected {
            now: at(0),
            peer: vsock(ONE),
        });
        observer.observe_actions(&[grant(ONE, 1, 3072 * MIB)]);
        observer.refresh(&broker);
        observer.device("one").reconnected();
    }
}
