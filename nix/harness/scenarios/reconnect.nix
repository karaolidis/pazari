{
  description = "a cleanly restarted agent rejoins its session and grows again";

  timeout = 420;

  daemon.policy = {
    host_floor = "1GiB";
    grant_min_interval = "2s";
  };

  guests.main = {
    daemon.ceiling = "4GiB";

    module = { ... }: {
      services.pazari-agent.harness.workload.phases = [
        "grow=1GiB,rate=256MiB/s"
        "hold=30s"
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
        connections = run.daemon.counter("pazarid_connections_total")

        run.require(
            grown >= size + BLOCK,
            "the guest is holding a plug the restart has to be reconciled against",
        )

        guest.shell(
            "systemctl restart pazari-agent.service",
            "the agent was restarted under a live session",
        )
        restarted = run.seen

        run.daemon.rose(
            "pazarid_connections_total",
            floor=connections + 1,
            timeout=120,
            reason="the daemon took the agent's new connection",
            since=restarted,
        )
        reconnected = run.seen

        run.daemon.reaches(
            "pazarid_guest_connected",
            lambda value: value == 1,
            120,
            "the guest is connected and past the handshake again",
            guest.labels,
            since=reconnected,
        )
        rejoined = run.seen

        guest.agent.reaches(
            "pazari_agent_connected",
            lambda value: value == 1,
            60,
            "the restarted agent is past the handshake again",
            since=rejoined,
        )
        guest.agent.reaches(
            "pazari_agent_ceiling_bytes",
            lambda value: value > 0,
            60,
            "the restarted agent was told the ceiling again",
            since=rejoined,
        )

        run.require(
            run.daemon.gauge("pazarid_guest_size_bytes", guest.labels) >= grown,
            "the restart cost the guest nothing: the device still holds what it held",
        )

        steady(run, guest, "the engine settled after the restart", since=rejoined)
        resumed = run.daemon.gauge("pazarid_guest_size_bytes", guest.labels)

        run.require(
            guest.agent.gauge("pazari_agent_memory_high_bytes")
            < guest.agent.gauge("pazari_agent_memory_max_bytes")
            <= guest.agent.gauge("pazari_agent_plugged_bytes"),
            "the restarted agent rebuilt the band from the plug it found",
        )

        run.daemon.reaches(
            "pazarid_guest_size_bytes",
            lambda value: value >= resumed + BLOCK,
            180,
            "the second burst won a grant over the rejoined session",
            guest.labels,
        )

        steady(run, guest, "the engine settled after the second grow", since=run.seen)

        run.require(
            run.daemon.held(
                "pazarid_guest_size_bytes",
                lambda value: value >= grown,
                over=10,
                labels=guest.labels,
                since=restarted,
            ),
            "nothing the guest held was taken back across the restart",
        )
  '';
}
