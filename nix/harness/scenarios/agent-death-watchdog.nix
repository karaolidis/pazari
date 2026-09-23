{
  description = "an agent killed under a braked workload fails open, comes back and re-brakes";

  timeout = 240;

  daemon = {
    policy = {
      host_floor = "2GiB";
      grant_min_interval = "2s";
    };

    guest.ceiling = "1792MiB";
  };

  guest =
    {
      config,
      lib,
      pkgs,
      ...
    }:
    {
      pazari.harness = {
        engine = {
          burst_reserve = "512MiB";
          open_guard = "384MiB";
        };

        workload.phases = [
          "grow=800MiB,rate=384MiB/s"
          "release=384MiB"
          "grow=384MiB,rate=384MiB/s"
          "hold=30s"
        ];
      };

      systemd.services.pazari-harness-reaper = {
        description = "pazari harness agent reaper";
        wantedBy = [ "multi-user.target" ];
        requires = [ "${config.pazari.harness.workload.unit}.service" ];
        after = [ "${config.pazari.harness.workload.unit}.service" ];

        serviceConfig = {
          Type = "oneshot";
          ExecStartPre = "${lib.getExe' pkgs.coreutils "sleep"} 20";
          ExecStart = "${lib.getExe' config.systemd.package "systemctl"} kill --kill-whom=main --signal=KILL pazari-agent.service";
        };
      };
    };

  assertions = ''
    from pazari_harness.checks import CRAWLING, collapsed, recovered

    GROW = 0
    REGROW = 2


    def check(run):
        guest = {"guest": run.scenario.guest}

        run.gauge_reaches(
            "agent",
            "pazari_agent_band_open",
            lambda value: value == 1,
            120,
            "the brake opened under the throttled workload",
        )
        run.counter_rose(
            "agent",
            "pazari_agent_denies_total",
            labels={"reason": "ceiling"},
            timeout=90,
            reason="the ceiling refused the grow, so the workload stays braked",
        )
        run.gauge_reaches(
            "agent",
            "pazari_agent_mode",
            lambda value: value == 1,
            90,
            "the engine is stalled at the ceiling",
            {"mode": "exhausted"},
        )

        collapsed(run, GROW, "the braked workload is crawling", share=CRAWLING)
        braked = run.seen

        died = run.answering(
            "agent",
            120,
            "the agent was killed and its metrics endpoint went away",
            ok=False,
            since=braked,
        )

        recovered(
            run,
            REGROW,
            "ExecStopPost released the brake and the workload ran unthrottled",
            since=died,
            timeout=90,
        )

        run.gauge_reaches(
            "daemon",
            "pazarid_guest_connected",
            lambda value: value == 0,
            90,
            "the daemon saw the guest drop",
            guest,
            since=braked,
        )
        dropped = run.seen

        run.answering("agent", 120, "the agent came back", since=died)

        run.gauge_reaches(
            "daemon",
            "pazarid_guest_connected",
            lambda value: value == 1,
            120,
            "the daemon saw the guest reconnect",
            guest,
            since=dropped,
        )

        run.gauge_reaches(
            "agent",
            "pazari_agent_mode",
            lambda value: value == 1,
            90,
            "the restarted agent found the same wall again",
            {"mode": "exhausted"},
            since=died,
        )

        run.require(
            run.gauge("agent", "pazari_agent_memory_high_bytes")
            < run.gauge("agent", "pazari_agent_memory_max_bytes"),
            "the brake is back on, under memory.max",
        )
  '';
}
