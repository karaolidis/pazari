{
  description = "an agent frozen mid-session goes stale without the session being torn down";

  timeout = 300;

  daemon.policy = {
    host_floor = "2GiB";
    grant_min_interval = "2s";
    stale_after = 3;
  };

  guests.main = {
    daemon.ceiling = "3GiB";

    module = { ... }: {
      # The agent is frozen on purpose here, and a watchdog would shoot it.
      services.pazari-agent.watchdog.enable = false;

      services.pazari-agent.harness.workload.phases = [
        "grow=1GiB,rate=256MiB/s"
        "hold=120s"
      ];
    };
  };

  assertions = ''
    from pazari_harness.checks import grew, steady

    FREEZE = "systemctl kill --kill-whom=main --signal=STOP pazari-agent.service"
    THAW = "systemctl kill --kill-whom=main --signal=CONT pazari-agent.service"

    def check(run):
        guest = run.guest

        size = run.daemon.gauge("pazarid_guest_size_bytes", guest.labels)

        grew(run, guest, size)
        steady(run, guest, "the engine settled with a session the host is watching")

        dropped = run.daemon.counter("pazarid_disconnections_total")
        held = run.daemon.gauge("pazarid_guest_size_bytes", guest.labels)

        guest.shell(FREEZE, "the agent was frozen mid-session")
        frozen = run.seen

        run.daemon.reaches(
            "pazarid_guest_stale",
            lambda value: value == 1,
            60,
            "the daemon noticed the guest had stopped sending status",
            guest.labels,
            since=frozen,
        )
        stale = run.seen

        run.require(
            run.daemon.gauge("pazarid_guest_connected", guest.labels) == 1,
            "the guest is stale, not gone: the daemon still holds the session",
        )
        run.require(
            run.daemon.counter("pazarid_disconnections_total") == dropped,
            "nothing was hung up on",
        )

        guest.shell(THAW, "the agent was let go again")
        thawed = run.seen

        run.daemon.reaches(
            "pazarid_guest_stale",
            lambda value: value == 0,
            60,
            "status resumed and the daemon took the guest off stale",
            guest.labels,
            since=thawed,
        )

        run.require(
            run.daemon.counter("pazarid_disconnections_total") == dropped,
            "the guest came back over the same connection, with no reconnect",
        )
        run.require(
            run.daemon.gauge("pazarid_guest_size_bytes", guest.labels) == held,
            "no memory moved while the daemon could not hear the guest",
        )

        run.require(
            stale - frozen < 30,
            f"the daemon called it in {stale - frozen:.1f}s, off three missed intervals",
        )

        steady(run, guest, "the engine carried on where it left off", since=thawed)
  '';
}
