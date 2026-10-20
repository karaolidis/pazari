{
  description = "a restarted daemon reads the guest's size back off the device and moves nothing";

  timeout = 480;

  daemon.settings.grant = {
    floor = "1GiB";
    min_interval = "2s";
  };

  guests.main = {
    daemon.ceiling = "4GiB";

    module = { ... }: {
      services.pazari-agent.harness.workload.phases = [
        "grow=1GiB,rate=256MiB/s"
        "hold=45s"
        "grow=1GiB,rate=256MiB/s"
        "hold=60s"
      ];
    };
  };

  assertions = ''
    from pazari_harness.checks import BLOCK, grew, steady

    def check(run):
        guest = run.guest

        size = run.daemon.gauge("pazarid_guest_size_bytes", guest.labels)

        grew(run, guest, size)
        steady(run, guest, "the engine settled after the first grow")

        grown = run.daemon.gauge("pazarid_guest_size_bytes", guest.labels)
        plugged = guest.agent.gauge("pazari_agent_total_mem_bytes")
        before = run.clock()

        run.require(
            grown >= size + BLOCK,
            "the guest holds a plug the daemon will have to reconcile against",
        )

        run.host.restart_daemon("the daemon was restarted under a live session")
        restarted = run.seen

        run.daemon.reaches(
            "pazarid_grants_total",
            lambda value: value == 0,
            90,
            "the daemon came back with its counters at zero, so it remembers nothing",
            since=restarted,
        )
        fresh = run.seen

        run.daemon.reaches(
            "pazarid_guest_size_bytes",
            lambda value: value == grown,
            90,
            "the fresh daemon read the guest's size back off the device",
            guest.labels,
            since=fresh,
        )
        run.require(
            run.daemon.counter("pazarid_device_seed_failures_total", guest.labels) == 0,
            "the device said what size it is without being asked twice",
        )

        run.daemon.reaches(
            "pazarid_guest_connected",
            lambda value: value == 1,
            150,
            "the guest found the fresh daemon and handshook again",
            guest.labels,
            since=fresh,
        )
        rejoined = run.seen

        steady(run, guest, "the engine rode the restart out in steady", since=rejoined)

        resumed = run.daemon.gauge("pazarid_guest_size_bytes", guest.labels)

        run.daemon.reaches(
            "pazarid_guest_size_bytes",
            lambda value: value >= resumed + BLOCK,
            180,
            "the next burst won a grant from the daemon that came back",
            guest.labels,
        )

        steady(run, guest, "the engine settled after the second grow", since=run.seen)

        run.require(
            guest.agent.held(
                "pazari_agent_total_mem_bytes",
                lambda value: value >= plugged,
                over=10,
                since=before,
            ),
            "no resize crossed the restart: the guest's MemTotal never dipped",
        )
  '';
}
