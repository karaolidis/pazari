{
  description = "a workload stalled at its ceiling crawls, is never killed, and comes back on its own";

  timeout = 300;

  daemon.settings.grant = {
    floor = "2GiB";
    min_interval = "2s";
  };

  guests.main = {
    daemon.ceiling = "1792MiB";

    module = { ... }: {
      services.pazari-agent.settings = {
        workload.burst_reserve = "512MiB";

        exhaustion.mode = {
          kind = "stall";
          timeout = "600s";
        };
      };

      services.pazari-agent.harness = {
        workload.phases = [
          "grow=850MiB,rate=96MiB/s"
          "hold=600s"
        ];
      };
    };
  };

  assertions = ''
    from pazari_harness.checks import CRAWLING, collapsed, steady

    GROW = 0

    THROTTLED = 5000

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
            "the engine is stalled at the ceiling",
            {"mode": "exhausted"},
        )
        stalled = run.seen
        events = guest.agent.counter("pazari_agent_high_events")

        guest.agent.rose(
            "pazari_agent_exhaustions_total",
            timeout=30,
            reason="the engine counted the exhaustion",
        )

        run.require(
            guest.agent.gauge("pazari_agent_memory_high_bytes")
            < guest.agent.gauge("pazari_agent_memory_max_bytes"),
            "the stall left the band open under memory.max, it did not collapse it",
        )

        collapsed(run, guest, GROW, "the stalled workload is crawling", share=CRAWLING)

        guest.agent.rose(
            "pazari_agent_high_events",
            floor=events + THROTTLED,
            timeout=180,
            reason="the kernel went on throttling the workload rather than killing it",
            since=stalled,
        )

        run.require(
            guest.agent.counter("pazari_agent_max_events") == 0,
            "nothing in the guest ever reached memory.max",
        )
        limit = guest.agent.gauge("pazari_agent_memory_max_bytes")

        run.wait_for(
            lambda: guest.agent.held(
                "pazari_agent_memory_high_bytes",
                lambda value: value < limit,
                over=20,
                since=stalled,
            ),
            120,
            "the brake stayed on, under memory.max, for the whole stall",
        )

        guest.shell(
            "systemctl is-active pazari-workload.service",
            "the stalled workload is still running",
        )
        guest.shell(
            "systemctl stop pazari-workload.service",
            "the workload was asked to let go of what it held",
            timeout=120,
        )

        steady(
            run,
            guest,
            "the engine came back to steady on its own once the demand went away",
            since=run.seen,
        )
  '';
}
