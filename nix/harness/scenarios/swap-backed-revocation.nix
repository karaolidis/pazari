{
  description = "a host driven into swap takes memory back from the guest holding the most";

  timeout = 600;

  daemon.settings = {
    grant = {
      floor = "512MiB";
      min_interval = "2s";
      swap = true;
    };

    reclaim = {
      deadline = "30s";

      revocation = {
        psi = 10.0;
        step = "512MiB";
      };
    };
  };

  guests.main = {
    daemon.ceiling = "2560MiB";

    module = { ... }: {
      # The idle guest must not volunteer the memory before the host asks.
      services.pazari-agent.settings.release = {
        kind = "titrate";
        cooldown = "600s";
      };

      services.pazari-agent.harness = {
        workload.phases = [
          "grow=1200MiB,rate=256MiB/s"
          "release=900MiB"
          "hold=600s"
        ];
      };
    };
  };

  host = { lib, pkgs, ... }: {
    zramSwap = {
      enable = true;
      memoryPercent = 100;
    };

    systemd.services.pazari-harness-hog = {
      description = "pazari harness host memory hog";

      serviceConfig = {
        ExecStart = lib.escapeShellArgs [
          (lib.getExe' pkgs.pazari "pazari-stress")
          "--interval"
          "5s"
          "--no-reserve"
          "grow=6GiB,rate=256MiB/s"
          "churn=300s,rate=1GiB/s"
        ];

        SyslogIdentifier = "pazari-harness-hog";
        OOMScoreAdjust = 500;
        Restart = "no";
      };
    };
  };

  assertions = ''
    from pazari_harness.checks import BLOCK, grew, steady

    PRESSED = 10.0

    HOG = "pazari-harness-hog.service"

    def check(run):
        guest = run.guest

        size = run.daemon.gauge("pazarid_guest_size_bytes", guest.labels)

        grew(run, guest, size)
        steady(run, guest, "the engine settled with the workload holding its peak")

        guest.agent.reaches(
            "pazari_agent_workload_usage_bytes",
            lambda value: value < 512 * 1024 * 1024,
            180,
            "the workload let go of most of what it held",
        )

        held = run.daemon.gauge("pazarid_guest_size_bytes", guest.labels)

        run.require(
            held >= size + BLOCK,
            "the guest is still holding a plug the host can ask back for",
        )

        asked = guest.agent.counter("pazari_agent_reclaim_asked_bytes_total")

        run.host.start(HOG, "a hog was set on the host's memory")
        hogged = run.seen

        run.daemon.reaches(
            "pazarid_host_psi",
            lambda value: value >= PRESSED,
            300,
            "the host crossed the pressure it revokes at",
            since=hogged,
        )
        pressed = run.seen

        run.daemon.rose(
            "pazarid_reclaim_requests_total",
            timeout=120,
            reason="the daemon asked the guest for memory back",
            since=pressed,
        )
        run.daemon.rose(
            "pazarid_guest_messages_total",
            labels={"kind": "reclaim_ack"},
            timeout=120,
            reason="the guest took the reclaim on",
            since=pressed,
        )

        guest.agent.rose(
            "pazari_agent_reclaims_total",
            timeout=120,
            reason="the agent ran memory.reclaim over the workload",
            since=pressed,
        )
        guest.agent.rose(
            "pazari_agent_reclaim_asked_bytes_total",
            floor=asked + BLOCK,
            timeout=120,
            reason="the reclaim asked the kernel for a block's worth or more",
            since=pressed,
        )
        guest.agent.rose(
            "pazari_agent_offers_total",
            timeout=120,
            reason="the agent offered the memory it had freed back",
            since=pressed,
        )

        run.daemon.reaches(
            "pazarid_guest_size_bytes",
            lambda value: value <= held - BLOCK,
            240,
            "the guest's device gave at least one block back",
            guest.labels,
            since=pressed,
        )
        guest.agent.reaches(
            "pazari_agent_total_mem_bytes",
            lambda value: value <= run.daemon.gauge(
                "pazarid_guest_size_bytes", guest.labels
            )
            + BLOCK,
            120,
            "the guest's MemTotal followed the device down",
            since=pressed,
        )

        run.require(
            run.daemon.counter(
                "pazarid_guest_messages_total", {"kind": "reclaim_nak"}
            )
            == 0,
            "the guest never refused, so the host never had to shun it",
        )

        run.host.stop(HOG, "the hog gave the host's memory back")
        freed = run.seen

        run.daemon.reaches(
            "pazarid_host_psi",
            lambda value: value < PRESSED,
            300,
            "the host's pressure relented once the memory came back",
            since=freed,
        )
  '';
}
