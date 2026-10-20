{
  description = "a burst past memory.high wins a grant, and the plug follows";

  timeout = 420;

  daemon.settings.grant = {
    floor = "2GiB";
    min_interval = "2s";
  };

  guests.main = {
    daemon.ceiling = "3GiB";

    module = { ... }: {
      services.pazari-agent.harness.workload.phases = [
        "grow=2GiB,rate=384MiB/s"
        "hold=20s"
      ];
    };
  };

  assertions = ''
    from pazari_harness.checks import BLOCK, collapsed, followed, grew, recovered, steady

    GROW = 0

    def check(run):
        guest = run.guest

        size = run.daemon.gauge("pazarid_guest_size_bytes", guest.labels)
        booted = guest.agent.gauge("pazari_agent_total_mem_bytes")

        grew(run, guest, size)
        followed(guest, booted + BLOCK)

        stalled = collapsed(
            run, guest, GROW, "the brake collapsed the workload's fault rate"
        )

        recovered(
            run,
            guest,
            GROW,
            "the workload came back to the pace its plan asked for",
            since=stalled[0]["at"],
        )

        steady(run, guest, "the engine settled back into steady")
  '';
}
