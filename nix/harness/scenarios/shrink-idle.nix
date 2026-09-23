{
  description = "a workload that grows and then goes quiet hands the memory back";

  timeout = 240;

  daemon = {
    policy = {
      host_floor = "2GiB";
      grant_min_interval = "2s";
    };

    guest.ceiling = "3GiB";
  };

  guest = { ... }: {
    pazari.harness.workload.phases = [
      "grow=2GiB,rate=384MiB/s"
      "release=1800MiB"
      "hold=90s"
    ];
  };

  assertions = ''
    from pazari_harness.checks import BLOCK, followed, grew, steady

    RETURNED = 512 * 1024 * 1024


    def check(run):
        guest = {"guest": run.scenario.guest}

        size = run.gauge("daemon", "pazarid_guest_size_bytes", guest)
        booted = run.gauge("agent", "pazari_agent_plugged_bytes")

        grew(run, guest, size)
        followed(run, booted + BLOCK)

        steady(run, "the engine settled after the grow")

        granted = run.counter("agent", "pazari_agent_grants_total")

        run.counter_rose(
            "agent",
            "pazari_agent_offers_total",
            timeout=150,
            reason="the idle workload made the agent offer memory back",
        )
        run.counter_rose(
            "daemon",
            "pazarid_guest_messages_total",
            labels={"kind": "offer_release"},
            timeout=60,
            reason="the daemon took the offer",
        )
        run.counter_rose(
            "agent",
            "pazari_agent_grants_total",
            floor=granted + 1,
            timeout=60,
            reason="the host granted the release",
            since=run.seen,
        )

        peak = run.daemon.peak("pazarid_guest_size_bytes", guest)
        plugged = run.agent.peak("pazari_agent_plugged_bytes")

        run.require(
            peak - size >= RETURNED,
            "the plug the run gave back was worth watching",
        )

        run.gauge_reaches(
            "daemon",
            "pazarid_guest_size_bytes",
            lambda value: value <= size,
            120,
            "the device handed back every block it was given",
            guest,
        )
        run.gauge_reaches(
            "agent",
            "pazari_agent_plugged_bytes",
            lambda value: value <= plugged - RETURNED,
            60,
            "the guest's MemTotal followed the device down",
        )

        steady(run, "the engine settled at the smaller plug", since=run.seen)
  '';
}
