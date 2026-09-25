{
  description = "an agent killed under a braked workload fails open, comes back and re-brakes";

  timeout = 240;

  daemon.policy = {
    host_floor = "2GiB";
    grant_min_interval = "2s";
  };

  guests.main = {
    daemon.ceiling = "1792MiB";

    module =
      {
        config,
        lib,
        pkgs,
        ...
      }:
      {
        services.pazari-agent.harness = {
          engine = {
            burst_reserve = "512MiB";
          };

          workload.phases = [
            "grow=800MiB,rate=384MiB/s"
            "release=384MiB"
            "grow=384MiB,rate=384MiB/s"
            "hold=180s"
          ];
        };

        systemd.services.pazari-harness-reaper = {
          description = "pazari harness agent reaper";
          wantedBy = [ "multi-user.target" ];
          requires = [ "${config.services.pazari-agent.harness.workload.unit}.service" ];
          after = [ "${config.services.pazari-agent.harness.workload.unit}.service" ];

          serviceConfig = {
            Type = "oneshot";
            ExecStartPre = "${lib.getExe' pkgs.coreutils "sleep"} 20";
            ExecStart = "${lib.getExe' config.systemd.package "systemctl"} kill --kill-whom=main --signal=KILL pazari-agent.service";
          };
        };
      };
  };

  assertions = ''
    from pazari_harness.checks import CRAWLING, collapsed, recovered

    GROW = 0
    REGROW = 2

    def check(run):
        guest = run.guest

        guest.agent.reaches(
            "pazari_agent_brake_released",
            lambda value: value == 1,
            120,
            "the brake opened under the throttled workload",
        )
        guest.agent.rose(
            "pazari_agent_denies_total",
            labels={"reason": "ceiling"},
            timeout=90,
            reason="the ceiling refused the grow, so the workload stays braked",
        )
        guest.agent.reaches(
            "pazari_agent_mode",
            lambda value: value == 1,
            90,
            "the engine is stalled at the ceiling",
            {"mode": "exhausted"},
        )

        collapsed(run, guest, GROW, "the braked workload is crawling", share=CRAWLING)
        braked = run.seen

        died = guest.agent.answering(
            120,
            "the agent was killed and its metrics endpoint went away",
            ok=False,
            since=braked,
        )

        recovered(
            run,
            guest,
            REGROW,
            "ExecStopPost released the brake and the workload ran unthrottled",
            since=died,
            timeout=90,
        )

        run.daemon.reaches(
            "pazarid_guest_connected",
            lambda value: value == 0,
            90,
            "the daemon saw the guest drop",
            guest.labels,
            since=braked,
        )
        dropped = run.seen

        guest.agent.answering(120, "the agent came back", since=died)

        run.daemon.reaches(
            "pazarid_guest_connected",
            lambda value: value == 1,
            120,
            "the daemon saw the guest reconnect",
            guest.labels,
            since=dropped,
        )

        guest.agent.reaches(
            "pazari_agent_mode",
            lambda value: value == 1,
            90,
            "the restarted agent found the same wall again",
            {"mode": "exhausted"},
            since=died,
        )

        run.require(
            guest.agent.gauge("pazari_agent_memory_high_bytes")
            < guest.agent.gauge("pazari_agent_memory_max_bytes"),
            "the brake is back on, under memory.max",
        )
  '';
}
