pub mod error;

mod file;

use std::{net::SocketAddr, path::PathBuf};

use pazari_transport::{AnyListener, TransportError, UnixListener, VsockListener};
use pazari_types::Interval;

pub use crate::config::{
    error::ConfigError,
    file::{RECONNECT_MAX, RECONNECT_MIN},
};
use crate::{
    broker::{GuestId, Policy, Registry},
    config::file::File,
    runtime::Mode,
};
pub use pazari_config::Settings;

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub tick_interval: Interval,

    pub listeners: Vec<Transport>,

    pub metrics: Option<SocketAddr>,

    pub policy: Policy,

    pub registry: Registry,

    /// Each registered guest's device.
    pub devices: Vec<(GuestId, Qmp)>,
}

impl Settings for Config {
    type File = File;
    type Error = ConfigError;

    fn resolve(file: File) -> Result<Self, ConfigError> {
        file.resolve()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    Vsock { cid: u32, port: u32 },

    Unix { path: PathBuf },
}

impl Transport {
    pub fn bind(&self) -> Result<AnyListener, TransportError> {
        match self {
            Self::Vsock { cid, port } => Ok(AnyListener::Vsock(VsockListener::bind(*cid, *port)?)),
            Self::Unix { path } => Ok(AnyListener::Unix(UnixListener::bind(path)?)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Qmp {
    pub path: PathBuf,

    pub device: String,

    pub deadline: Interval,

    pub reconnect_min: Interval,
    pub reconnect_max: Interval,

    pub mode: Mode,
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use std::{net::Ipv4Addr, time::Duration};

    use pazari_transport::{
        Listener, PeerId,
        adapter::vsock::{DEFAULT_VSOCK_PORT, VMADDR_CID_ANY},
    };
    use pazari_types::Psi;
    use tempfile::TempDir;

    use super::*;
    use crate::broker::{
        Grantable, GuestSpec, Revocation, StalledShrinks,
        testing::{bytes, count},
    };

    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;

    const FIXTURE: &str = "pazarid.toml";

    const EXAMPLE: &str = include_str!("../../../../examples/pazarid.toml");

    const FULL: &str = r#"
        [tick]
        interval = "500ms"

        [status]
        interval = "2s"
        stale_after = 5

        [grant]
        floor = "3GiB"
        min_interval = "10s"
        swap = true

        [actuation]
        deadline = "45s"
        revert_shrinks = false

        [reclaim]
        deadline = "20s"

        [reclaim.revocation]
        psi = 30.0
        step = "512MiB"

        [guests.example]
        ceiling = "8GiB"

        [guests.example.transport]
        kind = "unix"
        path = "/run/pazari/example.sock"

        [guests.example.qmp]
        path = "/run/pazari/example.qmp"
        device = "vm0"
        deadline = "2s 500ms"
        reconnect_min = "2s"
        reconnect_max = "60s"

        [guests.example.qmp.mode]
        kind = "per_operation"
        poll = "250ms"
    "#;

    const MINIMAL: &str = r#"
        [grant]
        floor = "2GiB"

        [guests.example]
        ceiling = "8GiB"

        [guests.example.transport]
        kind = "vsock"
        cid = 3

        [guests.example.qmp]
        path = "/run/pazari/example.qmp"
        device = "vm0"
    "#;

    const SECOND: &str = "[guests.other]\nceiling = \"4GiB\"\n\n\
                          [guests.other.transport]\nkind = \"vsock\"\ncid = 4\n\n\
                          [guests.other.qmp]\npath = \"/run/pazari/other.qmp\"\n\
                          device = \"vm1\"\n\n[guests.example]";

    const OTHER_UNIX: &str = "[guests.other]\nceiling = \"4GiB\"\n\n\
                              [guests.other.transport]\nkind = \"unix\"\n\
                              path = \"/run/pazari/other.sock\"\n\n\
                              [guests.other.qmp]\npath = \"/run/pazari/other.qmp\"\n\
                              device = \"vm1\"\n\n[guests.example]";

    fn interval(value: Duration) -> Interval {
        Interval::try_new(value).expect("a positive interval under a day")
    }

    fn parse(text: &str) -> Config {
        Config::parse(Path::new(FIXTURE), text).expect("the fixture is a workable configuration")
    }

    fn rejects(text: &str) -> String {
        Config::parse(Path::new(FIXTURE), text)
            .expect_err("the fixture is not a workable configuration")
            .to_string()
    }

    #[test]
    fn the_example_file_is_the_template_the_daemon_prints() {
        assert_eq!(
            EXAMPLE,
            Config::template(),
            "examples/pazarid.toml is stale: regenerate it with \
             `cargo run -p pazarid -- example-config > examples/pazarid.toml`"
        );
    }

    #[test]
    fn the_example_file_carries_the_defaults_it_stands_in_for() {
        let defaults = parse(MINIMAL);

        assert_eq!(
            parse(EXAMPLE),
            Config {
                policy: Policy {
                    revocation: Some(Revocation {
                        psi: Psi::try_new(20.0).expect("20% is a psi"),
                        step: bytes(256 * MIB),
                    }),
                    ..defaults.policy
                },
                metrics: Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 9650))),
                ..defaults
            },
            "the template's example values have drifted from the defaults they print"
        );
    }

    #[test]
    fn a_file_that_sets_everything_is_taken_at_its_word() {
        let config = parse(FULL);

        assert_eq!(config.tick_interval, interval(Duration::from_millis(500)));
        assert_eq!(
            config.listeners,
            [Transport::Unix {
                path: PathBuf::from("/run/pazari/example.sock"),
            }]
        );
        assert_eq!(
            config.policy,
            Policy {
                status_interval: interval(Duration::from_secs(2)),
                stale_after: count(5),
                grant_floor: 3 * GIB,
                grant_min_interval: interval(Duration::from_secs(10)),
                grantable: Grantable::RamAndSwap,
                actuation_deadline: interval(Duration::from_secs(45)),
                stalled_shrinks: StalledShrinks::Leave,
                reclaim_deadline: interval(Duration::from_secs(20)),
                revocation: Some(Revocation {
                    psi: Psi::try_new(30.0).expect("30% is a psi"),
                    step: bytes(512 * MIB),
                }),
            }
        );
        let peer = PeerId::Unix {
            path: PathBuf::from("/run/pazari/example.sock"),
        };
        let guest = config.registry.lookup(&peer).expect("a registered guest");

        assert_eq!(
            config.devices,
            vec![(
                guest,
                Qmp {
                    path: PathBuf::from("/run/pazari/example.qmp"),
                    device: "vm0".to_owned(),
                    deadline: interval(Duration::from_millis(2_500)),
                    reconnect_min: interval(Duration::from_secs(2)),
                    reconnect_max: interval(Duration::from_secs(60)),
                    mode: Mode::PerOperation {
                        poll: interval(Duration::from_millis(250)),
                    },
                }
            )]
        );
        assert_eq!(
            config.registry.spec(guest),
            Some(&GuestSpec {
                name: "example".to_owned(),
                ceiling: bytes(8 * GIB),
            })
        );
    }

    #[test]
    fn a_file_that_sets_nothing_optional_takes_every_default() {
        let config = parse(MINIMAL);

        assert_eq!(config.tick_interval, interval(Duration::from_secs(1)));
        assert_eq!(
            config.listeners,
            [Transport::Vsock {
                cid: VMADDR_CID_ANY,
                port: DEFAULT_VSOCK_PORT,
            }]
        );
        assert_eq!(
            config.policy,
            Policy {
                status_interval: interval(Duration::from_secs(1)),
                stale_after: count(3),
                grant_floor: 2 * GIB,
                grant_min_interval: interval(Duration::from_secs(5)),
                grantable: Grantable::Ram,
                actuation_deadline: interval(Duration::from_secs(30)),
                stalled_shrinks: StalledShrinks::Revert,
                reclaim_deadline: interval(Duration::from_secs(30)),
                revocation: None,
            }
        );

        let guest = config
            .registry
            .lookup(&PeerId::Vsock { cid: 3 })
            .expect("a registered guest");

        assert_eq!(
            config.devices,
            vec![(
                guest,
                Qmp {
                    path: PathBuf::from("/run/pazari/example.qmp"),
                    device: "vm0".to_owned(),
                    deadline: interval(Duration::from_secs(5)),
                    reconnect_min: interval(Duration::from_millis(500)),
                    reconnect_max: interval(Duration::from_secs(30)),
                    mode: Mode::Held,
                }
            )]
        );
    }

    #[test]
    fn guests_are_registered_in_the_order_their_tables_sort() {
        let config = parse(&MINIMAL.replace("[guests.example]", SECOND));

        assert_eq!(
            config
                .registry
                .iter()
                .map(|(_, spec)| spec.name.as_str())
                .collect::<Vec<_>>(),
            ["example", "other"]
        );
        assert_eq!(
            config
                .devices
                .iter()
                .map(|(guest, qmp)| (*guest, qmp.device.as_str()))
                .collect::<Vec<_>>(),
            [
                (GuestId::from_index(0), "vm0"),
                (GuestId::from_index(1), "vm1")
            ]
        );
    }

    #[test]
    fn guests_of_either_kind_stand_side_by_side() {
        let config = parse(&MINIMAL.replace("[guests.example]", OTHER_UNIX));

        assert_eq!(
            config.listeners,
            [
                Transport::Vsock {
                    cid: VMADDR_CID_ANY,
                    port: DEFAULT_VSOCK_PORT,
                },
                Transport::Unix {
                    path: PathBuf::from("/run/pazari/other.sock"),
                },
            ]
        );
        assert_eq!(
            config
                .registry
                .iter()
                .map(|(_, spec)| spec.name.as_str())
                .collect::<Vec<_>>(),
            ["example", "other"]
        );
    }

    #[test]
    fn every_vsock_guest_is_answered_by_one_listener_on_the_daemon_s_port() {
        let config = parse(
            &MINIMAL
                .replace("[guests.example]", SECOND)
                .replace("[grant]", "[vsock]\nport = 5000\n\n[grant]"),
        );

        assert_eq!(
            config.listeners,
            [Transport::Vsock {
                cid: VMADDR_CID_ANY,
                port: 5000,
            }]
        );
    }

    #[test]
    fn a_vsock_port_no_listener_can_take_is_refused() {
        for port in [0, u32::MAX] {
            let error =
                rejects(&MINIMAL.replace("[grant]", &format!("[vsock]\nport = {port}\n\n[grant]")));

            assert!(
                error.contains(&format!("vsock port {port} cannot be listened on")),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn a_port_on_one_guest_is_refused() {
        let error = rejects(&MINIMAL.replace("cid = 3\n", "cid = 3\nport = 5000\n"));

        assert!(error.contains("port"), "unexpected error: {error}");
    }

    #[test]
    fn a_context_id_vsock_keeps_for_itself_is_refused() {
        for cid in [0, 1, 2, u32::MAX] {
            let error = rejects(&MINIMAL.replace("cid = 3\n", &format!("cid = {cid}\n")));

            assert!(
                error.contains(&format!("context ID {cid} is reserved")),
                "unexpected error: {error}"
            );
        }

        assert!(
            parse(&MINIMAL.replace("cid = 3\n", "cid = 4294967294\n"))
                .registry
                .lookup(&PeerId::Vsock { cid: u32::MAX - 1 })
                .is_some()
        );
    }

    #[test]
    fn a_file_that_names_no_guests_is_refused() {
        let error = rejects("[grant]\nfloor = \"2GiB\"\n");

        assert_eq!(error, "the file names no guests");
    }

    #[test]
    fn a_vsock_guest_without_a_cid_is_refused() {
        let error = rejects(&MINIMAL.replace("cid = 3\n", ""));

        assert!(error.contains("cid"), "unexpected error: {error}");
    }

    #[test]
    fn a_unix_guest_with_a_cid_is_refused() {
        let error = rejects(&FULL.replace(
            r#"path = "/run/pazari/example.sock""#,
            "cid = 3\npath = \"/run/pazari/example.sock\"",
        ));

        assert!(error.contains("cid"), "unexpected error: {error}");
    }

    #[test]
    fn a_second_guest_at_one_context_id_is_refused() {
        let error =
            rejects(&MINIMAL.replace("[guests.example]", &SECOND.replace("cid = 4", "cid = 3")));

        assert_eq!(error, "vsock:3 is already registered as example");
    }

    #[test]
    fn a_second_guest_at_one_socket_is_refused() {
        let error = rejects(&FULL.replace(
            "[guests.example]",
            &OTHER_UNIX.replace("/run/pazari/other.sock", "/run/pazari/example.sock"),
        ));

        assert_eq!(
            error,
            "unix:/run/pazari/example.sock is already registered as example"
        );
    }

    #[test]
    fn a_malformed_duration_names_the_field_that_carries_it() {
        let error = rejects(&MINIMAL.replace(
            r#"floor = "2GiB""#,
            "floor = \"2GiB\"\nmin_interval = \"3 fortnights\"",
        ));

        assert!(error.contains("min_interval"), "unexpected error: {error}");
    }

    #[test]
    fn a_malformed_size_names_the_field_that_carries_it() {
        let error = rejects(&MINIMAL.replace(r#"floor = "2GiB""#, r#"floor = "2 lorries""#));

        assert!(error.contains("floor"), "unexpected error: {error}");
    }

    #[test]
    fn an_unknown_key_names_itself() {
        let error = rejects(&MINIMAL.replace("[grant]", "[grant]\nnagle = true"));

        assert!(error.contains("nagle"), "unexpected error: {error}");
    }

    #[test]
    fn an_unknown_enum_names_the_field_that_carries_it() {
        let error = rejects(&MINIMAL.replace(r#"kind = "vsock""#, r#"kind = "carrier_pigeon""#));

        assert!(error.contains("kind"), "unexpected error: {error}");
        assert!(error.contains("vsock"), "unexpected error: {error}");
    }

    #[test]
    fn a_unix_transport_without_a_path_is_refused() {
        let error =
            rejects(&MINIMAL.replace("kind = \"vsock\"\n        cid = 3", "kind = \"unix\""));

        assert!(error.contains("path"), "unexpected error: {error}");
    }

    #[test]
    fn a_socket_path_on_a_vsock_transport_is_refused() {
        let error = rejects(&MINIMAL.replace(
            r#"kind = "vsock""#,
            "kind = \"vsock\"\npath = \"/run/pazari/example.sock\"",
        ));

        assert!(error.contains("path"), "unexpected error: {error}");
    }

    #[test]
    fn a_poll_interval_on_a_held_socket_is_refused() {
        let error = rejects(&MINIMAL.replace(
            r#"device = "vm0""#,
            "device = \"vm0\"\n\n[guests.example.qmp.mode]\nkind = \"held\"\npoll = \"1s\"",
        ));

        assert!(error.contains("poll"), "unexpected error: {error}");
    }

    #[test]
    fn a_zero_interval_names_the_field_that_carries_it() {
        let error = rejects(&MINIMAL.replace("[grant]", "[tick]\ninterval = \"0s\"\n\n[grant]"));

        assert_eq!(
            error,
            "tick.interval: Interval rejects 0ns: expected (0s, 24h]"
        );
    }

    #[test]
    fn a_zero_count_names_the_field_that_carries_it() {
        let error = rejects(&MINIMAL.replace("[grant]", "[status]\nstale_after = 0\n\n[grant]"));

        assert_eq!(
            error,
            "status.stale_after rejects 0: expected a positive count of intervals"
        );
    }

    #[test]
    fn a_zero_ceiling_names_the_guest_that_carries_it() {
        let error = rejects(&MINIMAL.replace(r#"ceiling = "8GiB""#, r#"ceiling = "0B""#));

        assert_eq!(
            error,
            "guests.example.ceiling rejects 0: expected a positive byte count"
        );
    }

    #[test]
    fn a_zero_deadline_names_the_guest_that_carries_it() {
        let error =
            rejects(&MINIMAL.replace(r#"device = "vm0""#, "device = \"vm0\"\ndeadline = \"0s\""));

        assert_eq!(
            error,
            "guests.example.qmp.deadline: Interval rejects 0ns: expected (0s, 24h]"
        );
    }

    #[test]
    fn a_file_that_asks_for_no_metrics_binds_nothing() {
        assert_eq!(parse(MINIMAL).metrics, None);
    }

    #[test]
    fn a_metrics_table_binds_the_address_it_names() {
        assert_eq!(
            parse(&MINIMAL.replace(
                "[grant]",
                "[metrics]\nlisten = \"127.0.0.1:9650\"\n\n[grant]",
            ))
            .metrics,
            Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 9650)))
        );
    }

    #[test]
    fn a_metrics_address_that_is_not_one_names_the_field_that_carries_it() {
        let error = rejects(&MINIMAL.replace(
            "[grant]",
            "[metrics]\nlisten = \"nowhere in particular\"\n\n[grant]",
        ));

        assert!(error.contains("listen"), "unexpected error: {error}");
    }

    #[test]
    fn a_metrics_table_without_an_address_is_refused() {
        let error = rejects(&MINIMAL.replace("[grant]", "[metrics]\n\n[grant]"));

        assert!(error.contains("listen"), "unexpected error: {error}");
    }

    #[test]
    fn a_file_that_asks_for_no_revocation_gets_none() {
        assert_eq!(parse(MINIMAL).policy.revocation, None);
    }

    #[test]
    fn a_revocation_table_without_a_threshold_names_the_field() {
        let error = rejects(&FULL.replace("psi = 30.0\n", ""));

        assert!(error.contains("psi"), "unexpected error: {error}");
    }

    #[test]
    fn a_revocation_table_takes_the_default_step() {
        let config = parse(&FULL.replace("step = \"512MiB\"\n", ""));

        assert_eq!(
            config.policy.revocation.map(|revocation| revocation.step),
            Some(bytes(256 * MIB))
        );
    }

    #[test]
    fn a_revocation_threshold_outside_its_range_is_refused() {
        assert_eq!(
            rejects(&FULL.replace("psi = 30.0", "psi = 101.0")),
            "reclaim.revocation.psi: Psi rejects 101.0: expected (0, 100]"
        );
    }

    #[test]
    fn a_revocation_that_asks_for_nothing_is_refused() {
        assert_eq!(
            rejects(&FULL.replace(r#"step = "512MiB""#, r#"step = "0B""#)),
            "reclaim.revocation.step rejects 0: expected a positive byte count"
        );
    }

    #[test]
    fn a_reconnect_floor_over_its_own_cap_is_refused() {
        assert_eq!(
            rejects(&FULL.replace(r#"reconnect_min = "2s""#, r#"reconnect_min = "90s""#)),
            "guests.example.qmp.reconnect_min is 90s, over guests.example.qmp.reconnect_max of 60s"
        );
    }

    #[test]
    fn a_missing_file_names_itself() {
        let path = PathBuf::from("/nonexistent/pazari/pazarid.toml");
        let error = Config::load(&path).expect_err("the file is not there");

        assert!(
            error
                .to_string()
                .contains("/nonexistent/pazari/pazarid.toml"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn the_listeners_bind_what_the_file_named() {
        let dir = TempDir::new().expect("a temporary directory");
        let path = dir.path().join("example.sock");

        let config = parse(&FULL.replace("/run/pazari/example.sock", &path.to_string_lossy()));

        let bound = config
            .listeners
            .iter()
            .map(|transport| transport.bind().expect("a bound listener").local())
            .collect::<Vec<_>>();

        assert_eq!(bound, [PeerId::Unix { path }]);
    }
}
