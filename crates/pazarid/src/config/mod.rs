pub mod error;

mod file;

use std::path::{Path, PathBuf};

use pazari_transport::{AnyListener, TransportError, UnixListener, VsockListener};
use pazari_types::Interval;

pub use crate::config::error::ConfigError;
use crate::{
    broker::{Policy, Registry},
    config::file::File,
    runtime::Mode,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub tick_interval: Interval,

    pub listeners: Vec<Transport>,

    pub policy: Policy,

    pub registry: Registry,

    pub devices: Vec<Qmp>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: PathBuf::from(path),
            source,
        })?;

        Self::parse(path, &text)
    }

    pub fn parse(path: &Path, text: &str) -> Result<Self, ConfigError> {
        toml::from_str::<File>(text)
            .map_err(|source| ConfigError::Syntax {
                path: PathBuf::from(path),
                source,
            })?
            .resolve()
    }

    #[must_use]
    pub fn template() -> String {
        let formatting = doku::toml::Formatting {
            auto_comments: doku::toml::AutoComments {
                optional: false,
                ..Default::default()
            },
            enums_style: doku::toml::EnumsStyle::Commented,
            ..Default::default()
        };

        let printed = doku::to_toml_fmt::<File>(&formatting);

        // doku pads the blank comment lines between enum variant fields.
        let mut template = printed
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n");
        template.push('\n');

        template
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

    pub mode: Mode,

    pub deadline: Interval,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pazari_proto::DEFAULT_VSOCK_PORT;
    use pazari_transport::{Listener, PeerId, adapter::vsock::VMADDR_CID_ANY};
    use tempfile::TempDir;

    use super::*;
    use crate::broker::{GuestSpec, testing::count};

    const GIB: u64 = 1 << 30;

    const FIXTURE: &str = "pazarid.toml";

    const EXAMPLE: &str = include_str!("../../../../docs/pazarid.example.toml");

    const FULL: &str = r#"
        tick_interval = "500ms"

        [policy]
        welcome_status_interval = "2s"
        host_floor = "3GiB"
        grant_min_interval = "10s"
        reconcile_deadline = "45s"
        shrink_revert = false
        stale_after = 5
        reclaim_deadline = "20s"

        [guests.example]
        ceiling = "8GiB"

        [guests.example.transport]
        kind = "unix"
        path = "/run/pazari/example.sock"

        [guests.example.qmp]
        path = "/run/pazari/example.qmp"
        device = "vm0"
        deadline = "2s 500ms"

        [guests.example.qmp.mode]
        kind = "per_operation"
        poll = "250ms"
    "#;

    const MINIMAL: &str = r#"
        [policy]
        host_floor = "2GiB"

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
            "docs/pazarid.example.toml is stale: regenerate it with \
             `cargo run -p pazarid -- example-config > docs/pazarid.example.toml`"
        );
    }

    #[test]
    fn the_example_file_carries_the_defaults_it_stands_in_for() {
        assert_eq!(
            parse(EXAMPLE),
            parse(MINIMAL),
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
                welcome_status_interval: interval(Duration::from_secs(2)),
                host_floor: 3 * GIB,
                grant_min_interval: interval(Duration::from_secs(10)),
                reconcile_deadline: interval(Duration::from_secs(45)),
                shrink_revert: false,
                stale_after: count(5),
                reclaim_deadline: interval(Duration::from_secs(20)),
            }
        );
        assert_eq!(
            config.devices,
            vec![Qmp {
                path: PathBuf::from("/run/pazari/example.qmp"),
                device: "vm0".to_owned(),
                mode: Mode::PerOperation {
                    poll: interval(Duration::from_millis(250)),
                },
                deadline: interval(Duration::from_millis(2_500)),
            }]
        );

        let peer = PeerId::Unix {
            path: PathBuf::from("/run/pazari/example.sock"),
        };
        let guest = config.registry.lookup(&peer).expect("a registered guest");

        assert_eq!(
            config.registry.spec(guest),
            Some(&GuestSpec {
                name: "example".to_owned(),
                ceiling: 8 * GIB,
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
                welcome_status_interval: interval(Duration::from_secs(1)),
                host_floor: 2 * GIB,
                grant_min_interval: interval(Duration::from_secs(5)),
                reconcile_deadline: interval(Duration::from_secs(30)),
                shrink_revert: true,
                stale_after: count(3),
                reclaim_deadline: interval(Duration::from_secs(30)),
            }
        );
        assert_eq!(
            config.devices,
            vec![Qmp {
                path: PathBuf::from("/run/pazari/example.qmp"),
                device: "vm0".to_owned(),
                mode: Mode::Held,
                deadline: interval(Duration::from_secs(5)),
            }]
        );
        assert!(config.registry.lookup(&PeerId::Vsock { cid: 3 }).is_some());
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
                .map(|qmp| qmp.device.as_str())
                .collect::<Vec<_>>(),
            ["vm0", "vm1"]
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
    fn guests_sharing_a_port_are_answered_by_one_listener() {
        let config = parse(&MINIMAL.replace("[guests.example]", SECOND));

        assert_eq!(
            config.listeners,
            [Transport::Vsock {
                cid: VMADDR_CID_ANY,
                port: DEFAULT_VSOCK_PORT,
            }]
        );
    }

    #[test]
    fn a_file_that_names_no_guests_is_refused() {
        let error = rejects("[policy]\nhost_floor = \"2GiB\"\n");

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
            r#"host_floor = "2GiB""#,
            "host_floor = \"2GiB\"\ngrant_min_interval = \"3 fortnights\"",
        ));

        assert!(
            error.contains("grant_min_interval"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn a_malformed_size_names_the_field_that_carries_it() {
        let error =
            rejects(&MINIMAL.replace(r#"host_floor = "2GiB""#, r#"host_floor = "2 lorries""#));

        assert!(error.contains("host_floor"), "unexpected error: {error}");
    }

    #[test]
    fn an_unknown_key_names_itself() {
        let error = rejects(&MINIMAL.replace("[policy]", "[policy]\nnagle = true"));

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
        let error = rejects(&MINIMAL.replace("[policy]", "tick_interval = \"0s\"\n\n[policy]"));

        assert_eq!(
            error,
            "tick_interval: Interval rejects 0ns: expected (0s, 24h]"
        );
    }

    #[test]
    fn a_zero_count_names_the_field_that_carries_it() {
        let error = rejects(&MINIMAL.replace(
            r#"host_floor = "2GiB""#,
            "host_floor = \"2GiB\"\nstale_after = 0",
        ));

        assert_eq!(
            error,
            "policy.stale_after rejects 0: expected a positive count of intervals"
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
