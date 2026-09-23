{
  description = "a burst past memory.high wins a grant, and the plug follows";

  timeout = 180;

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
      "hold=20s"
    ];
  };

  assertions = ''
    from pazari_harness.checks import BLOCK, collapsed, followed, grew, recovered, steady

    GROW = 0


    def check(run):
        guest = {"guest": run.scenario.guest}

        size = run.gauge("daemon", "pazarid_guest_size_bytes", guest)
        plugged = run.gauge("agent", "pazari_agent_plugged_bytes")

        grew(run, guest, size)
        followed(run, plugged + BLOCK)

        stalled = collapsed(run, GROW, "the brake collapsed the workload's fault rate")

        recovered(
            run,
            GROW,
            "the workload came back to the pace its plan asked for",
            since=stalled[0]["at"],
        )

        steady(run, "the engine settled back into steady")
  '';
}
