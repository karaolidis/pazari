{
  description = "a workload that fills the page cache is reclaimed from, not grown into, when throttles reclaim first";

  timeout = 600;

  daemon.settings.grant = {
    floor = "1GiB";
    min_interval = "2s";
  };

  guests =
    let
      scratch = "/var/cache/scratch";

      guest = throttle: {
        daemon.ceiling = "3GiB";

        module = { ... }: {
          microvm.volumes = [
            {
              image = "scratch.img";
              mountPoint = scratch;
              size = 4096;
            }
          ];

          systemd.services.pazari-workload = {
            requires = [ "var-cache-scratch.mount" ];
            after = [ "var-cache-scratch.mount" ];
          };

          services.pazari-agent.settings = { inherit throttle; };

          services.pazari-agent.harness = {
            workload = {
              arguments = [
                "--interval"
                "250ms"
                "--no-reserve"
                "--cache-dir"
                scratch
              ];

              phases = [
                "grow=256MiB"
                "cache=3GiB,rate=96MiB/s"
                "hold=30s"
              ];
            };
          };
        };
      };
    in
    {
      growing = guest { kind = "grow"; };

      reclaiming = guest {
        kind = "reclaim";
        amount = "256MiB";
      };
    };

  assertions = ''
    from pazari_harness.units import MIB

    MARGIN = 512 * MIB

    def check(run):
        growing = run.guests["growing"]
        reclaiming = run.guests["reclaiming"]

        started = {
            guest.name: run.daemon.gauge("pazarid_guest_size_bytes", guest.labels)
            for guest in (growing, reclaiming)
        }

        for guest in (growing, reclaiming):
            run.wait_for(
                lambda guest=guest: guest.workload.of("done"),
                480,
                f"{guest.name}'s workload wrote its page cache and finished",
            )

        peaks = {
            guest.name: run.daemon.peak("pazarid_guest_size_bytes", guest.labels)
            for guest in (growing, reclaiming)
        }

        run.note("peaks", started=started, peaks=peaks)

        run.require(
            peaks["growing"] - started["growing"] >= MARGIN,
            "the growing guest grew into its page cache, so there is a difference to show",
        )
        run.require(
            reclaiming.agent.counter("pazari_agent_throttle_reclaims_total") > 0,
            "the reclaiming guest answered throttles by reclaiming",
        )
        run.require(
            peaks["reclaiming"] + MARGIN <= peaks["growing"],
            "the reclaiming guest grew less than the growing one",
        )
  '';
}
