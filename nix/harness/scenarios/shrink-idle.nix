{
  description = "a workload that grows and then goes quiet hands the memory back";

  timeout = 240;

  daemon.policy = {
    host_floor = "2GiB";
    grant_min_interval = "2s";
  };

  guests.main = {
    daemon.ceiling = "3GiB";

    module = { ... }: {
      services.pazari-agent.harness.workload.phases = [
        "grow=2GiB,rate=384MiB/s"
        "release=1800MiB"
        "hold=90s"
      ];
    };
  };

  assertions = ''
    from pazari_harness.checks import BLOCK, followed, grew, steady

    RETURNED = 512 * 1024 * 1024

    def check(run):
        guest = run.guest

        size = run.daemon.gauge("pazarid_guest_size_bytes", guest.labels)
        booted = guest.agent.gauge("pazari_agent_total_mem_bytes")

        grew(run, guest, size)
        followed(guest, booted + BLOCK)

        steady(run, guest, "the engine settled after the grow")

        granted = guest.agent.counter("pazari_agent_grants_total")

        guest.agent.rose(
            "pazari_agent_offers_total",
            timeout=150,
            reason="the idle workload made the agent offer memory back",
        )
        run.daemon.rose(
            "pazarid_guest_messages_total",
            labels={"kind": "offer_release"},
            timeout=60,
            reason="the daemon took the offer",
        )
        guest.agent.rose(
            "pazari_agent_grants_total",
            floor=granted + 1,
            timeout=60,
            reason="the host granted the release",
            since=run.seen,
        )

        peak = run.daemon.peak("pazarid_guest_size_bytes", guest.labels)
        plugged = guest.agent.peak("pazari_agent_total_mem_bytes")

        run.require(
            peak - size >= RETURNED,
            "the plug the run gave back was worth watching",
        )

        run.daemon.reaches(
            "pazarid_guest_size_bytes",
            lambda value: value <= size,
            120,
            "the device handed back every block it was given",
            guest.labels,
        )
        guest.agent.reaches(
            "pazari_agent_total_mem_bytes",
            lambda value: value <= plugged - RETURNED,
            60,
            "the guest's MemTotal followed the device down",
        )

        steady(run, guest, "the engine settled at the smaller plug", since=run.seen)
  '';
}
