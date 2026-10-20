mod file;

use std::net::SocketAddr;

pub use pazari_config::{ConfigError, Settings};

use crate::{
    config::file::File, engine::EngineConfig, runtime::RuntimeConfig, session::SessionConfig,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub engine: EngineConfig,
    pub runtime: RuntimeConfig,
    pub session: SessionConfig,
    pub metrics: Option<SocketAddr>,
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
    use std::{
        net::Ipv4Addr,
        path::{Path, PathBuf},
    };

    use pazari_transport::{
        AnyConnector, UnixConnector, VsockConnector,
        adapter::vsock::{DEFAULT_VSOCK_PORT, VMADDR_CID_HOST},
    };
    use pazari_types::{Delay, Swappiness};

    use super::*;
    use crate::{
        engine::{
            ExhaustionMode, ExponentialConfig, GrowConfig, ReleaseConfig, SweepConfig,
            ThrottleMode, TitrateConfig,
            testing::{MIB, bytes, count, delay, interval, interval_ms},
        },
        runtime::{Backend, Paths, ReclaimConfig, ThpAlways},
    };

    const FIXTURE: &str = "agent.toml";

    const EXAMPLE: &str = include_str!("../../../../examples/agent.toml");

    const FULL: &str = r#"
        [tick]
        interval = "500ms"

        [workload]
        cgroup = "/sys/fs/cgroup/system.slice/workload.service"
        service_reserve = "0B"
        burst_reserve = "192MiB"

        [workload.backend]
        kind = "systemd"
        unit = "workload.service"

        [host]
        handshake_timeout = "3s"
        reconnect_min = "100ms"
        reconnect_max = "2s"

        [host.transport]
        kind = "unix"
        path = "/run/pazari/host.sock"

        [resize]
        settle_grace = "15s"
        deny_backoff = "2s 500ms"
        hotplug_debounce = "1s"

        [reclaim]
        chunk = "16MiB"
        swappiness = 0

        [throttle]
        kind = "reclaim"
        amount = "128MiB"

        [grow]
        kind = "exponential"
        step = "1GiB"
        escalate_within = "20s"
        cap = 16

        [release]
        kind = "titrate"
        step = "128MiB"
        after = 5
        cooldown = "0s"

        [exhaustion]
        recover_after = 5

        [exhaustion.mode]
        kind = "kill"

        [preflight]
        thp_always = "warn"
    "#;

    const MINIMAL: &str = r#"
        [workload]
        cgroup = "/sys/fs/cgroup/workload.slice"
        service_reserve = "256MiB"
        burst_reserve = "256MiB"

        [host.transport]
        kind = "vsock"
    "#;

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
            "examples/agent.toml is stale: regenerate it with \
             `cargo run -p pazari-agent -- example-config > examples/agent.toml`"
        );
    }

    #[test]
    fn the_example_file_differs_from_the_defaults_only_in_its_optional_keys() {
        let defaults = parse(MINIMAL);

        assert_eq!(
            parse(EXAMPLE),
            Config {
                runtime: RuntimeConfig {
                    reclaim: ReclaimConfig {
                        swappiness: Some(Swappiness::NONE),
                        ..defaults.runtime.reclaim
                    },
                    ..defaults.runtime.clone()
                },
                metrics: Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 9651))),
                ..defaults
            },
            "the template sets a key away from its default that is not optional"
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
                "[host.transport]",
                "[metrics]\nlisten = \"127.0.0.1:9651\"\n\n[host.transport]",
            ))
            .metrics,
            Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 9651)))
        );
    }

    #[test]
    fn a_metrics_address_that_is_not_one_names_the_field_that_carries_it() {
        let error = rejects(&MINIMAL.replace(
            "[host.transport]",
            "[metrics]\nlisten = \"nowhere in particular\"\n\n[host.transport]",
        ));

        assert!(error.contains("listen"), "unexpected error: {error}");
    }

    #[test]
    fn a_metrics_table_without_an_address_is_refused() {
        let error = rejects(&MINIMAL.replace("[host.transport]", "[metrics]\n\n[host.transport]"));

        assert!(error.contains("listen"), "unexpected error: {error}");
    }

    #[test]
    fn a_file_that_sets_everything_is_taken_at_its_word() {
        let config = parse(FULL);

        assert_eq!(
            config.engine,
            EngineConfig {
                service_reserve: 0,
                burst_reserve: bytes(192 * MIB),
                throttle: ThrottleMode::Reclaim {
                    amount: bytes(128 * MIB),
                },
                grow: GrowConfig::Exponential(ExponentialConfig {
                    step: bytes(1024 * MIB),
                    escalate_within: interval(20),
                    cap: count(16),
                }),
                release: ReleaseConfig::Titrate(TitrateConfig {
                    step: bytes(128 * MIB),
                    after: count(5),
                    cooldown: Delay::ZERO,
                }),
                settle_grace: interval(15),
                deny_backoff: interval_ms(2_500),
                recover_after: count(5),
                exhaustion: ExhaustionMode::Kill,
            }
        );

        assert_eq!(
            config.runtime,
            RuntimeConfig {
                tick_interval: interval_ms(500),
                cgroup: PathBuf::from("/sys/fs/cgroup/system.slice/workload.service"),
                backend: Backend::Systemd {
                    unit: "workload.service".to_owned(),
                },
                hotplug_debounce: interval(1),
                reclaim: ReclaimConfig {
                    chunk: bytes(16 * MIB),
                    swappiness: Some(Swappiness::NONE),
                },
                thp_always: ThpAlways::Warn,
                paths: Paths::default(),
            }
        );

        assert_eq!(
            config.session,
            SessionConfig {
                handshake_timeout: interval(3),
                reconnect_min: interval_ms(100),
                reconnect_max: interval(2),
                connector: AnyConnector::Unix(UnixConnector::new("/run/pazari/host.sock")),
            }
        );
    }

    #[test]
    fn a_file_that_sets_nothing_optional_takes_every_default() {
        let config = parse(MINIMAL);

        assert_eq!(
            config.engine,
            EngineConfig {
                service_reserve: 256 * MIB,
                burst_reserve: bytes(256 * MIB),
                throttle: ThrottleMode::Grow,
                grow: GrowConfig::Exponential(ExponentialConfig {
                    step: bytes(512 * MIB),
                    escalate_within: interval(10),
                    cap: count(64),
                }),
                release: ReleaseConfig::Titrate(TitrateConfig {
                    step: bytes(256 * MIB),
                    after: count(3),
                    cooldown: delay(30),
                }),
                settle_grace: interval(10),
                deny_backoff: interval(5),
                recover_after: count(3),
                exhaustion: ExhaustionMode::Stall {
                    timeout: interval(60),
                },
            }
        );

        assert_eq!(
            config.runtime,
            RuntimeConfig {
                tick_interval: interval(1),
                cgroup: PathBuf::from("/sys/fs/cgroup/workload.slice"),
                backend: Backend::Cgroupfs,
                hotplug_debounce: interval_ms(250),
                reclaim: ReclaimConfig {
                    chunk: bytes(64 * MIB),
                    swappiness: None,
                },
                thp_always: ThpAlways::Fail,
                paths: Paths::default(),
            }
        );

        assert_eq!(
            config.session,
            SessionConfig {
                handshake_timeout: interval(10),
                reconnect_min: interval_ms(500),
                reconnect_max: interval(30),
                connector: AnyConnector::Vsock(VsockConnector::new(
                    VMADDR_CID_HOST,
                    DEFAULT_VSOCK_PORT
                )),
            }
        );
    }

    #[test]
    fn an_unknown_hugepage_posture_is_refused() {
        let error = rejects(&MINIMAL.replace(
            "[host.transport]",
            "[preflight]\nthp_always = \"shrug\"\n\n[host.transport]",
        ));

        assert!(error.contains("fail"), "unexpected error: {error}");
        assert!(error.contains("warn"), "unexpected error: {error}");
    }

    #[test]
    fn a_vsock_transport_takes_the_cid_and_port_the_file_names() {
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
            "[host.transport]",
            "[resize]\nhotplug_debounce = \"3 fortnights\"\n\n[host.transport]",
        ));

        assert!(
            error.contains("hotplug_debounce"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn a_malformed_size_names_the_field_that_carries_it() {
        let error = rejects(&MINIMAL.replace(
            r#"burst_reserve = "256MiB""#,
            r#"burst_reserve = "2 lorries""#,
        ));

        assert!(error.contains("burst_reserve"), "unexpected error: {error}");
    }

    #[test]
    fn an_unknown_enum_names_the_field_that_carries_it() {
        let error = rejects(&MINIMAL.replace(r#"kind = "vsock""#, r#"kind = "carrier_pigeon""#));

        assert!(error.contains("kind"), "unexpected error: {error}");
        assert!(error.contains("vsock"), "unexpected error: {error}");
    }

    #[test]
    fn an_unknown_key_names_itself() {
        let error = rejects(&MINIMAL.replace("[host.transport]", "[host.transport]\nnagle = true"));

        assert!(error.contains("nagle"), "unexpected error: {error}");
    }

    #[test]
    fn a_quantity_outside_its_range_names_the_field_and_the_range() {
        let error = rejects(&MINIMAL.replace(
            "[host.transport]",
            "[reclaim]\nswappiness = 201\n\n[host.transport]",
        ));

        assert_eq!(
            error,
            "reclaim.swappiness: Swappiness rejects 201: expected [0, 200]"
        );
    }

    #[test]
    fn a_zero_byte_count_names_the_field_that_carries_it() {
        let error =
            rejects(&MINIMAL.replace(r#"burst_reserve = "256MiB""#, r#"burst_reserve = "0B""#));

        assert_eq!(
            error,
            "workload.burst_reserve rejects 0: expected a positive byte count"
        );
    }

    #[test]
    fn a_zero_interval_names_the_field_that_carries_it() {
        let error =
            rejects(&MINIMAL.replace("[workload]", "[tick]\ninterval = \"0s\"\n\n[workload]"));

        assert_eq!(
            error,
            "tick.interval: Interval rejects 0ns: expected (0s, 24h]"
        );
    }

    #[test]
    fn a_systemd_backend_without_a_unit_is_refused() {
        let error = rejects(&MINIMAL.replace(
            "[host.transport]",
            "[workload.backend]\nkind = \"systemd\"\n\n[host.transport]",
        ));

        assert!(error.contains("unit"), "unexpected error: {error}");
    }

    #[test]
    fn a_unit_without_the_systemd_backend_is_refused() {
        let error = rejects(&MINIMAL.replace(
            "[host.transport]",
            "[workload.backend]\nkind = \"cgroupfs\"\nunit = \"workload.service\"\n\n[host.transport]",
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
            "[host.transport]",
            "[exhaustion.mode]\nkind = \"kill\"\ntimeout = \"30s\"\n\n[host.transport]",
        ));

        assert!(error.contains("timeout"), "unexpected error: {error}");
    }

    #[test]
    fn a_strategy_table_that_names_only_its_kind_takes_its_defaults() {
        let config = parse(&MINIMAL.replace(
            "[host.transport]",
            "[grow]\nkind = \"exponential\"\n\n\
             [release]\nkind = \"titrate\"\n\n[host.transport]",
        ));

        assert_eq!(config.engine, parse(MINIMAL).engine);
    }

    #[test]
    fn a_strategy_table_keeps_the_defaults_it_does_not_override() {
        let config = parse(&MINIMAL.replace(
            "[host.transport]",
            "[release]\nkind = \"titrate\"\nstep = \"64MiB\"\n\n[host.transport]",
        ));

        assert_eq!(
            config.engine.release,
            ReleaseConfig::Titrate(TitrateConfig {
                step: bytes(64 * MIB),
                after: count(3),
                cooldown: delay(30),
            })
        );
    }

    #[test]
    fn a_sweep_table_takes_the_defaults_it_does_not_override() {
        let config = parse(&MINIMAL.replace(
            "[host.transport]",
            "[release]\nkind = \"sweep\"\nhold = \"2m\"\n\n[host.transport]",
        ));

        assert_eq!(
            config.engine.release,
            ReleaseConfig::Sweep(SweepConfig {
                min: bytes(256 * MIB),
                hold: delay(120),
                cooldown: delay(30),
            })
        );
    }

    #[test]
    fn a_zero_sweep_minimum_names_the_table_that_carries_it() {
        let error = rejects(&MINIMAL.replace(
            "[host.transport]",
            "[release]\nkind = \"sweep\"\nmin = \"0B\"\n\n[host.transport]",
        ));

        assert_eq!(
            error,
            "release.min rejects 0: expected a positive byte count"
        );
    }

    #[test]
    fn a_titrate_key_on_a_sweep_is_refused() {
        let error = rejects(&MINIMAL.replace(
            "[host.transport]",
            "[release]\nkind = \"sweep\"\nafter = 3\n\n[host.transport]",
        ));

        assert!(error.contains("after"), "unexpected error: {error}");
    }

    #[test]
    fn a_reclaim_throttle_takes_the_default_amount() {
        let config = parse(&MINIMAL.replace(
            "[host.transport]",
            "[throttle]\nkind = \"reclaim\"\n\n[host.transport]",
        ));

        assert_eq!(
            config.engine.throttle,
            ThrottleMode::Reclaim {
                amount: bytes(256 * MIB),
            }
        );
    }

    #[test]
    fn a_zero_throttle_amount_names_the_table_that_carries_it() {
        let error = rejects(&MINIMAL.replace(
            "[host.transport]",
            "[throttle]\nkind = \"reclaim\"\namount = \"0B\"\n\n[host.transport]",
        ));

        assert_eq!(
            error,
            "throttle.amount rejects 0: expected a positive byte count"
        );
    }

    #[test]
    fn a_grow_table_keeps_the_cap_it_does_not_override() {
        let config = parse(&MINIMAL.replace(
            "[host.transport]",
            "[grow]\nkind = \"exponential\"\nstep = \"1GiB\"\n\n[host.transport]",
        ));

        assert_eq!(
            config.engine.grow,
            GrowConfig::Exponential(ExponentialConfig {
                step: bytes(1024 * MIB),
                escalate_within: interval(10),
                cap: count(64),
            })
        );
    }

    #[test]
    fn a_zero_grow_cap_names_the_table_that_carries_it() {
        let error = rejects(&MINIMAL.replace(
            "[host.transport]",
            "[grow]\nkind = \"exponential\"\ncap = 0\n\n[host.transport]",
        ));

        assert_eq!(
            error,
            "grow.cap rejects 0: expected a positive count of steps"
        );
    }

    #[test]
    fn a_strategy_table_without_a_kind_is_refused() {
        let error = rejects(&MINIMAL.replace(
            "[host.transport]",
            "[grow]\nstep = \"1GiB\"\n\n[host.transport]",
        ));

        assert!(error.contains("kind"), "unexpected error: {error}");
    }

    #[test]
    fn an_unknown_strategy_names_the_ones_there_are() {
        let error = rejects(&MINIMAL.replace(
            "[host.transport]",
            "[release]\nkind = \"all_at_once\"\n\n[host.transport]",
        ));

        assert!(error.contains("titrate"), "unexpected error: {error}");
    }

    #[test]
    fn a_key_from_another_strategy_is_refused() {
        let error = rejects(&MINIMAL.replace(
            "[host.transport]",
            "[grow]\nkind = \"exponential\"\nafter = 3\n\n[host.transport]",
        ));

        assert!(error.contains("after"), "unexpected error: {error}");
    }

    #[test]
    fn a_flat_key_from_before_the_strategy_tables_is_refused() {
        let error = rejects(&MINIMAL.replace(
            r#"burst_reserve = "256MiB""#,
            "burst_reserve = \"256MiB\"\nshrink_step = \"256MiB\"",
        ));

        assert!(error.contains("shrink_step"), "unexpected error: {error}");
    }

    #[test]
    fn a_zero_strategy_step_names_the_table_that_carries_it() {
        let error = rejects(&MINIMAL.replace(
            "[host.transport]",
            "[release]\nkind = \"titrate\"\nstep = \"0B\"\n\n[host.transport]",
        ));

        assert_eq!(
            error,
            "release.step rejects 0: expected a positive byte count"
        );
    }

    #[test]
    fn a_reconnect_floor_over_the_cap_is_refused() {
        let error = rejects(&MINIMAL.replace(
            "[host.transport]",
            "[host]\nreconnect_min = \"10s\"\nreconnect_max = \"1s\"\n\n[host.transport]",
        ));

        assert_eq!(
            error,
            "host.reconnect_min is 10s, over host.reconnect_max of 1s"
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
