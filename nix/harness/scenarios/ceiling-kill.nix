{
  description = "a workload past its ceiling under the kill posture is killed as a unit, and the guest lives";

  timeout = 420;

  breaches = true;

  daemon.policy = {
    host_floor = "2GiB";
    grant_min_interval = "2s";
  };

  guests.main = {
    daemon.ceiling = "1792MiB";

    module = { ... }: {
      services.pazari-agent.harness = {
        engine = {
          burst_reserve = "512MiB";
          open_guard = "384MiB";

          exhaustion.kind = "kill";
        };

        workload.phases = [
          "grow=1600MiB,rate=256MiB/s"
          "hold=60s"
        ];
      };
    };
  };

  assertions = ''
    from pazari_harness.checks import steady
    from pazari_harness.units import MIB

    EMPTY = 64 * MIB

    def check(run):
        guest = run.guest

        guest.agent.rose(
            "pazari_agent_denies_total",
            labels={"reason": "ceiling"},
            timeout=120,
            reason="the ceiling refused the grow",
        )
        guest.agent.reaches(
            "pazari_agent_mode",
            lambda value: value == 1,
            90,
            "the engine is out of room at the ceiling",
            {"mode": "exhausted"},
        )
        exhausted = run.seen

        run.require(
            guest.agent.gauge("pazari_agent_memory_high_bytes")
            == guest.agent.gauge("pazari_agent_memory_max_bytes"),
            "the kill posture collapsed the band onto memory.max",
        )

        guest.agent.rose(
            "pazari_agent_max_events",
            timeout=180,
            reason="the workload reached memory.max",
            since=exhausted,
        )

        guest.agent.reaches(
            "pazari_agent_workload_usage_bytes",
            lambda value: value < EMPTY,
            180,
            "the workload's cgroup was emptied",
            since=exhausted,
        )

        _, result = guest.shell(
            "systemctl show --property=Result --value pazari-workload.service",
            "systemd was asked what became of the workload",
        )

        run.require(
            result.strip() == "oom-kill",
            f"systemd calls the workload's end {result.strip()!r}, and the kill posture means oom-kill",
        )

        guest.shell(
            "systemctl is-active pazari-agent.service",
            "the agent outlived the workload it was braking",
        )
        run.require(
            guest.agent.gauge("pazari_agent_connected") == 1,
            "the agent is still connected to the host",
        )

        steady(
            run,
            guest,
            "the engine came back to steady over the empty cgroup",
            since=exhausted,
        )
  '';
}
