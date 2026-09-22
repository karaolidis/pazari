mod file;

pub use pazari_config::{ConfigError, Settings};

use crate::{
    config::file::File, engine::EngineConfig, runtime::RuntimeConfig, session::SessionConfig,
};

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub engine: EngineConfig,
    pub runtime: RuntimeConfig,
    pub session: SessionConfig,
}

impl Settings for Config {
    type File = File;
    type Error = ConfigError;

    fn resolve(file: File) -> Result<Self, ConfigError> {
        file.resolve()
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use std::{
        num::{NonZeroU32, NonZeroU64},
        time::Duration,
    };

    use pazari_proto::DEFAULT_VSOCK_PORT;
    use pazari_transport::{
        AnyConnector, UnixConnector, VsockConnector, adapter::vsock::VMADDR_CID_HOST,
    };
    use pazari_types::{Delay, Interval, Psi, Swappiness};

    use super::*;
    use crate::{
        engine::{ExhaustionMode, IdleThreshold},
        runtime::Backend,
    };

    const MIB: u64 = 1 << 20;

    const FIXTURE: &str = "agent.toml";

    const EXAMPLE: &str = include_str!("../../../../docs/agent.example.toml");

    const FULL: &str = r#"
        tick_interval = "500ms"

        [engine]
        service_reserve = "0B"
        burst_reserve = "192MiB"
        open_guard = "32MiB"
        grow_step = "1GiB"
        deny_backoff = "2s 500ms"
        settle_grace = "15s"
        idle_threshold = 0.75
        idle_intervals = 5
        shrink_step = "128MiB"
        shrink_cooldown = "0s"
        reclaim_swappiness = 0
        titrate_step = "32MiB"
        titrate_interval = "5s"
        titrate_psi_threshold = 12.5

        [engine.exhaustion]
        kind = "kill"

        [runtime]
        cgroup = "/sys/fs/cgroup/system.slice/workload.service"
        boot_mem = "4GiB"
        hotplug_debounce = "1s"
        reclaim_chunk = "16MiB"
        thp_always_fatal = false

        [runtime.backend]
        kind = "systemd"
        unit = "workload.service"

        [transport]
        kind = "unix"
        path = "/run/pazari/host.sock"
        handshake_timeout = "3s"
        reconnect_min = "100ms"
        reconnect_max = "2s"
    "#;

    const MINIMAL: &str = r#"
        [engine]
        service_reserve = "256MiB"
        burst_reserve = "256MiB"
        open_guard = "64MiB"

        [runtime]
        cgroup = "/sys/fs/cgroup/workload.slice"
        boot_mem = "2GiB"

        [transport]
        kind = "vsock"
    "#;

    fn bytes(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).expect("a positive byte count")
    }

    fn count(value: u32) -> NonZeroU32 {
        NonZeroU32::new(value).expect("a positive count")
    }

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
    fn the_example_file_is_the_template_the_agent_prints() {
        assert_eq!(
            EXAMPLE,
            Config::template(),
            "docs/agent.example.toml is stale: regenerate it with \
             `cargo run -p pazari-agent -- example-config > docs/agent.example.toml`"
        );
    }

    #[test]
    fn the_example_file_carries_the_defaults_it_stands_in_for() {
        let defaults = parse(MINIMAL);

        assert_eq!(
            parse(EXAMPLE),
            Config {
                engine: EngineConfig {
                    reclaim_swappiness: Some(Swappiness::NONE),
                    ..defaults.engine
                },
                ..defaults
            },
            "the template's example values have drifted from the defaults they print"
        );
    }

    #[test]
    fn a_file_that_sets_everything_is_taken_at_its_word() {
        let config = parse(FULL);

        assert_eq!(
            config.engine,
            EngineConfig {
                service_reserve: 0,
                burst_reserve: bytes(192 * MIB),
                open_guard: bytes(32 * MIB),
                grow_step: bytes(1024 * MIB),
                deny_backoff: interval(Duration::from_millis(2_500)),
                settle_grace: interval(Duration::from_secs(15)),
                idle_threshold: IdleThreshold::try_new(0.75).expect("three quarters is a deadband"),
                idle_intervals: count(5),
                shrink_step: bytes(128 * MIB),
                shrink_cooldown: Delay::ZERO,
                reclaim_swappiness: Some(Swappiness::NONE),
                titrate_step: bytes(32 * MIB),
                titrate_interval: interval(Duration::from_secs(5)),
                titrate_psi_threshold: Psi::try_new(12.5).expect("12.5% is a psi"),
                exhaustion: ExhaustionMode::Kill,
                tick_interval: interval(Duration::from_millis(500)),
            }
        );

        assert_eq!(
            config.runtime,
            RuntimeConfig {
                backend: Backend::Systemd {
                    unit: "workload.service".to_owned(),
                },
                tick_interval: interval(Duration::from_millis(500)),
                hotplug_debounce: interval(Duration::from_secs(1)),
                reclaim_chunk: bytes(16 * MIB),
                thp_always_fatal: false,
                ..RuntimeConfig::new(
                    "/sys/fs/cgroup/system.slice/workload.service",
                    bytes(4096 * MIB)
                )
            }
        );

        assert_eq!(
            config.session,
            SessionConfig {
                connector: AnyConnector::Unix(UnixConnector::new("/run/pazari/host.sock")),
                handshake_timeout: interval(Duration::from_secs(3)),
                reconnect_min: interval(Duration::from_millis(100)),
                reconnect_max: interval(Duration::from_secs(2)),
            }
        );
    }

    #[test]
    fn a_file_that_sets_nothing_optional_takes_every_default() {
        let config = parse(MINIMAL);

        assert_eq!(
            config.runtime,
            RuntimeConfig::new("/sys/fs/cgroup/workload.slice", bytes(2048 * MIB)),
            "the runtime defaults have drifted from RuntimeConfig::new"
        );

        assert_eq!(
            config.engine,
            EngineConfig {
                service_reserve: 256 * MIB,
                burst_reserve: bytes(256 * MIB),
                open_guard: bytes(64 * MIB),
                grow_step: bytes(512 * MIB),
                deny_backoff: interval(Duration::from_secs(5)),
                settle_grace: interval(Duration::from_secs(10)),
                idle_threshold: IdleThreshold::try_new(0.5).expect("a half is a deadband"),
                idle_intervals: count(3),
                shrink_step: bytes(256 * MIB),
                shrink_cooldown: Delay::try_new(Duration::from_secs(30)).expect("a short delay"),
                reclaim_swappiness: None,
                titrate_step: bytes(64 * MIB),
                titrate_interval: interval(Duration::from_secs(10)),
                titrate_psi_threshold: Psi::try_new(5.0).expect("5% is a psi"),
                exhaustion: ExhaustionMode::Stall {
                    timeout: interval(Duration::from_secs(60)),
                },
                tick_interval: interval(Duration::from_secs(1)),
            }
        );

        assert_eq!(
            config.session,
            SessionConfig {
                connector: AnyConnector::Vsock(VsockConnector::new(
                    VMADDR_CID_HOST,
                    DEFAULT_VSOCK_PORT
                )),
                handshake_timeout: interval(Duration::from_secs(10)),
                reconnect_min: interval(Duration::from_millis(500)),
                reconnect_max: interval(Duration::from_secs(30)),
            }
        );
    }

    #[test]
    fn the_vsock_address_is_whatever_the_file_leaves_out() {
        let config = parse(&MINIMAL.replace(
            r#"kind = "vsock""#,
            "kind = \"vsock\"\ncid = 7\nport = 9000",
        ));

        assert_eq!(
            config.session.connector,
            AnyConnector::Vsock(VsockConnector::new(7, 9_000))
        );
    }

    #[test]
    fn a_malformed_duration_names_the_field_that_carries_it() {
        let error = rejects(&MINIMAL.replace(
            r#"boot_mem = "2GiB""#,
            "boot_mem = \"2GiB\"\nhotplug_debounce = \"3 fortnights\"",
        ));

        assert!(
            error.contains("hotplug_debounce"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn a_malformed_size_names_the_field_that_carries_it() {
        let error = rejects(&MINIMAL.replace(r#"boot_mem = "2GiB""#, r#"boot_mem = "2 lorries""#));

        assert!(error.contains("boot_mem"), "unexpected error: {error}");
    }

    #[test]
    fn an_unknown_enum_names_the_field_that_carries_it() {
        let error = rejects(&MINIMAL.replace(r#"kind = "vsock""#, r#"kind = "carrier_pigeon""#));

        assert!(error.contains("kind"), "unexpected error: {error}");
        assert!(error.contains("vsock"), "unexpected error: {error}");
    }

    #[test]
    fn an_unknown_key_names_itself() {
        let error = rejects(&MINIMAL.replace("[transport]", "[transport]\nnagle = true"));

        assert!(error.contains("nagle"), "unexpected error: {error}");
    }

    #[test]
    fn a_quantity_outside_its_range_names_the_field_and_the_range() {
        let error = rejects(&MINIMAL.replace("[runtime]", "idle_threshold = 0.95\n\n[runtime]"));

        assert_eq!(
            error,
            "engine.idle_threshold: IdleThreshold rejects 0.95: expected (0, 0.9]"
        );
    }

    #[test]
    fn a_zero_byte_count_names_the_field_that_carries_it() {
        let error =
            rejects(&MINIMAL.replace(r#"burst_reserve = "256MiB""#, r#"burst_reserve = "0B""#));

        assert_eq!(
            error,
            "engine.burst_reserve rejects 0: expected a positive byte count"
        );
    }

    #[test]
    fn a_zero_interval_names_the_field_that_carries_it() {
        let error = rejects(&MINIMAL.replace("[engine]", "tick_interval = \"0s\"\n\n[engine]"));

        assert_eq!(
            error,
            "tick_interval: Interval rejects 0ns: expected (0s, 24h]"
        );
    }

    #[test]
    fn a_systemd_backend_without_a_unit_is_refused() {
        let error = rejects(&MINIMAL.replace(
            "[transport]",
            "[runtime.backend]\nkind = \"systemd\"\n\n[transport]",
        ));

        assert!(error.contains("unit"), "unexpected error: {error}");
    }

    #[test]
    fn a_unit_without_the_systemd_backend_is_refused() {
        let error = rejects(&MINIMAL.replace(
            "[transport]",
            "[runtime.backend]\nkind = \"cgroupfs\"\nunit = \"workload.service\"\n\n[transport]",
        ));

        assert!(error.contains("unit"), "unexpected error: {error}");
    }

    #[test]
    fn a_unix_transport_without_a_path_is_refused() {
        let error = rejects(&MINIMAL.replace(r#"kind = "vsock""#, r#"kind = "unix""#));

        assert!(error.contains("path"), "unexpected error: {error}");
    }

    #[test]
    fn a_vsock_address_on_a_unix_transport_is_refused() {
        let error = rejects(&MINIMAL.replace(
            r#"kind = "vsock""#,
            "kind = \"unix\"\npath = \"/run/pazari/host.sock\"\ncid = 2",
        ));

        assert!(error.contains("cid"), "unexpected error: {error}");
        assert!(error.contains("path"), "unexpected error: {error}");
    }

    #[test]
    fn a_socket_path_on_a_vsock_transport_is_refused() {
        let error = rejects(&MINIMAL.replace(
            r#"kind = "vsock""#,
            "kind = \"vsock\"\npath = \"/run/pazari/host.sock\"",
        ));

        assert!(error.contains("path"), "unexpected error: {error}");
        assert!(error.contains("cid"), "unexpected error: {error}");
    }

    #[test]
    fn a_stall_timeout_on_a_killed_workload_is_refused() {
        let error = rejects(&MINIMAL.replace(
            "[transport]",
            "[engine.exhaustion]\nkind = \"kill\"\ntimeout = \"30s\"\n\n[transport]",
        ));

        assert!(error.contains("timeout"), "unexpected error: {error}");
    }

    #[test]
    fn a_reconnect_floor_over_the_cap_is_refused() {
        let error = rejects(&MINIMAL.replace(
            r#"kind = "vsock""#,
            "kind = \"vsock\"\nreconnect_min = \"10s\"\nreconnect_max = \"1s\"",
        ));

        assert_eq!(
            error,
            "transport.reconnect_min is 10s, over transport.reconnect_max of 1s"
        );
    }

    #[test]
    fn a_file_that_names_no_cgroup_is_refused() {
        let error = rejects(&MINIMAL.replace(r#"cgroup = "/sys/fs/cgroup/workload.slice""#, ""));

        assert!(error.contains("cgroup"), "unexpected error: {error}");
    }

    #[test]
    fn a_missing_file_names_itself() {
        let path = PathBuf::from("/nonexistent/pazari/agent.toml");
        let error = Config::load(&path).expect_err("the file is not there");

        assert!(
            error.to_string().contains("/nonexistent/pazari/agent.toml"),
            "unexpected error: {error}"
        );
    }
}
