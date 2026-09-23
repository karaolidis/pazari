{
  description = "a host with nothing to spare denies the grow, and the guest waits out the delay it was given";

  timeout = 600;

  daemon.policy = {
    host_floor = "3GiB";
    grant_min_interval = "8s";
  };

  guests.main = {
    daemon.ceiling = "3GiB";

    module = { ... }: {
      services.pazari-agent.harness.workload.phases = [
        "grow=2GiB,rate=48MiB/s"
        "hold=120s"
      ];
    };
  };

  host = { lib, pkgs, ... }: {
    systemd.services.pazari-harness-hog = {
      description = "pazari harness host memory hog";
      wantedBy = [ "multi-user.target" ];

      serviceConfig = {
        ExecStart = lib.escapeShellArgs [
          (lib.getExe' pkgs.pazari "pazari-stress")
          "--interval"
          "5s"
          "--no-reserve"
          "grow=3GiB,rate=1GiB/s"
          "hold=3600s"
        ];

        SyslogIdentifier = "pazari-harness-hog";
        Restart = "no";
      };
    };
  };

  assertions = ''
    from pazari_harness.checks import BLOCK
    from pazari_harness.series import gaps, rises
    from pazari_harness.units import GIB

    SPACED = 6.0

    HOG = "pazari-harness-hog.service"

    def check(run):
        guest = run.guest

        run.daemon.reaches(
            "pazarid_host_available_bytes",
            lambda value: value < 4 * GIB,
            120,
            "the hog is holding the host's memory down",
        )

        guest.agent.rose(
            "pazari_agent_grant_requests_total",
            timeout=120,
            reason="the guest asked the host for more memory",
        )
        guest.agent.rose(
            "pazari_agent_denies_total",
            labels={"reason": "host_pressure"},
            timeout=120,
            reason="the host refused, with nothing left over its floor",
        )
        run.daemon.rose(
            "pazarid_denies_total",
            labels={"reason": "host_pressure"},
            timeout=60,
            reason="the daemon counted the refusal under the same reason",
        )

        guest.agent.rose(
            "pazari_agent_denies_total",
            floor=3,
            labels={"reason": "host_pressure"},
            timeout=180,
            reason="the guest kept asking, and kept being refused",
        )

        asked = [
            at
            for at in rises(guest.agent.totals("pazari_agent_grant_requests_total"))
            if at >= run.origin
        ]
        spacing = gaps(asked)

        run.require(
            len(spacing) >= 2,
            f"the guest retried more than once: it asked at {[round(at, 1) for at in asked]}",
        )
        run.require(
            min(spacing) >= SPACED,
            "every retry waited out the delay the host named: the closest two asks "
            f"were {min(spacing):.1f}s apart",
        )

        run.require(
            guest.agent.counter("pazari_agent_max_events") == 0,
            "the refused workload stayed braked under memory.max, and was never killed",
        )
        guest.shell(
            "systemctl is-active pazari-workload.service",
            "the refused workload is still running",
        )

        held = run.daemon.gauge("pazarid_guest_size_bytes", guest.labels)
        granted = guest.agent.counter("pazari_agent_grants_total")

        run.host.stop(HOG, "the hog gave the host's memory back")
        freed = run.seen

        run.daemon.reaches(
            "pazarid_host_available_bytes",
            lambda value: value > 4 * GIB,
            120,
            "the host has room over its floor again",
            since=freed,
        )

        guest.agent.rose(
            "pazari_agent_grants_total",
            floor=granted + 1,
            timeout=180,
            reason="the next retry landed a grant",
            since=freed,
        )
        run.daemon.reaches(
            "pazarid_guest_size_bytes",
            lambda value: value >= held + BLOCK,
            180,
            "the guest's device grew once the host could afford it",
            guest.labels,
            since=freed,
        )
  '';
}
