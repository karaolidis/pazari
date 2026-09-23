{
  description = "two guests share one daemon, and one of them dying leaves the other negotiating";

  timeout = 900;

  daemon.policy = {
    host_floor = "1GiB";
    grant_min_interval = "2s";
  };

  guests =
    let
      guest = phases: {
        daemon.ceiling = "2560MiB";

        module = { ... }: {
          microvm = {
            mem = 1024;
            hotplugMem = 2048;
          };

          services.pazari-agent.harness.workload.phases = phases;
        };
      };
    in
    {
      alpha = guest [
        "grow=1200MiB,rate=32MiB/s"
        "hold=180s"
      ];

      beta = guest [
        "grow=768MiB,rate=192MiB/s"
        "hold=300s"
      ];
    };

  assertions = ''
    from pazari_harness.checks import BLOCK, grew

    def check(run):
        alpha = run.guests["alpha"]
        beta = run.guests["beta"]

        started = {
            guest.name: run.daemon.gauge("pazarid_guest_size_bytes", guest.labels)
            for guest in (alpha, beta)
        }

        for guest in (alpha, beta):
            grew(run, guest, started[guest.name])

        for guest in (alpha, beta):
            run.require(
                run.daemon.gauge("pazarid_guest_ceiling_bytes", guest.labels) > 0,
                f"the daemon holds {guest.name} to a ceiling of its own",
            )
            run.require(
                run.daemon.gauge("pazarid_guest_connected", guest.labels) == 1,
                f"the daemon has {guest.name} connected",
            )

        grants = alpha.agent.counter("pazari_agent_grants_total")
        held = run.daemon.gauge("pazarid_guest_size_bytes", alpha.labels)

        beta.halt("beta's hypervisor was taken away from under the daemon")
        stopped = run.seen

        run.daemon.reaches(
            "pazarid_guest_connected",
            lambda value: value == 0,
            120,
            "the daemon saw beta go",
            beta.labels,
            since=stopped,
        )

        alpha.agent.rose(
            "pazari_agent_grants_total",
            floor=grants + 1,
            timeout=240,
            reason="alpha won another grant after beta died",
            since=stopped,
        )
        run.daemon.reaches(
            "pazarid_guest_size_bytes",
            lambda value: value >= held + BLOCK,
            240,
            "alpha's device grew again over the surviving session",
            alpha.labels,
            since=stopped,
        )

        run.require(
            run.daemon.gauge("pazarid_guest_connected", alpha.labels) == 1,
            "alpha never lost its own session",
        )
        run.require(
            alpha.agent.counter("pazari_agent_disconnections_total") == 0,
            "alpha was never disconnected, so beta's death never reached it",
        )
  '';
}
