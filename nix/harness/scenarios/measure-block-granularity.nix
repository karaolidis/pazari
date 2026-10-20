{
  description = "what the virtio-mem block size costs a grow, and whether a shrink still lands";

  timeout = 720;

  sample = 0.05;

  daemon.settings.grant = {
    floor = "1GiB";
    min_interval = "500ms";
  };

  guests =
    let
      arm = size: {
        daemon.ceiling = "3584MiB";

        module =
          { config, pkgs, ... }:
          let
            rewrite = pkgs.writeShellScript "qemu-virtio-mem-block-${size}" ''
              args=()

              for arg in "$@"; do
                case "$arg" in
                  virtio-mem-pci,*) arg="$arg,block-size=${size}" ;;
                esac

                args+=("$arg")
              done

              exec ${config.microvm.vmHostPackages.qemu_kvm}/bin/qemu-system-x86_64 \
                "''${args[@]}"
            '';
          in
          {
            microvm = {
              hotplugMem = 2048;

              qemu.package =
                pkgs.runCommand "qemu-virtio-mem-block-${size}"
                  { passthru = { inherit (config.microvm.vmHostPackages.qemu_kvm) configureFlags; }; }
                  ''
                    mkdir -p "$out/bin"
                    ln -s ${config.microvm.vmHostPackages.qemu_kvm}/bin/* "$out/bin/"
                    rm "$out/bin/qemu-system-x86_64"
                    ln -s ${rewrite} "$out/bin/qemu-system-x86_64"
                  '';
            };

            services.pazari-agent.settings = {
              resize = {
                settle_grace = "3s";
                deny_backoff = "1s";
              };

              grow = {
                kind = "exponential";
                step = "128MiB";
              };

              release = {
                kind = "titrate";
                cooldown = "2s";
              };
            };

            services.pazari-agent.harness = {
              workload.phases = [
                "grow=2GiB,rate=24MiB/s"
                "release=1536MiB"
                "hold=150s"
              ];
            };
          };
      };
    in
    {
      fine = arm "2M";
      coarse = arm "128M";
    };

  assertions = ''
    from pazari_harness.measure import bars, cdf, cycles, quantiles, record, save
    from pazari_harness.units import MIB

    import matplotlib.pyplot as plt

    ARMS = [("fine", "2MiB blocks"), ("coarse", "128MiB blocks")]

    ROUNDS = 5

    PATIENCE = 480

    SANE = 30000

    def measure(run, name):
        guest = run.guests[name]

        run.wait_for(
            lambda: guest.workload.of("done"),
            PATIENCE,
            f"{name}'s workload finished its plan",
            when=lambda events: events[0]["at"],
        )

        rounds = cycles(guest)
        peak = run.daemon.peak("pazarid_guest_size_bytes", guest.labels)
        final = run.daemon.gauge("pazarid_guest_size_bytes", guest.labels)

        return {
            "rounds": rounds,
            "round_trip_ms": quantiles([1000 * cycle["round_trip"] for cycle in rounds]),
            "device_peak_bytes": peak,
            "device_final_bytes": final,
            "device_returned_bytes": peak - final,
            "refused_actuations": run.daemon.counter(
                "pazarid_device_refused_actuations_total", guest.labels
            ),
        }

    def check(run):
        taken = {name: measure(run, name) for name, _ in ARMS}

        for name, _ in ARMS:
            record(run, name, {key: value for key, value in taken[name].items() if key != "rounds"})

        figure, axes = plt.subplots(1, 2, figsize=(13, 4.8), dpi=100)
        spread, returned = axes

        cdf(
            spread,
            [
                (label, [1000 * cycle["round_trip"] for cycle in taken[name]["rounds"]])
                for name, label in ARMS
            ],
            "the grow round trip under each block size",
            "request to MemTotal (milliseconds)",
        )

        bars(
            returned,
            [label for _, label in ARMS],
            [
                ("the device at its widest", [taken[name]["device_peak_bytes"] for name, _ in ARMS]),
                ("the device at the end", [taken[name]["device_final_bytes"] for name, _ in ARMS]),
            ],
            "what an idle guest managed to hand back",
            "virtio-mem block size",
            "memory device size (MiB)",
            scale=MIB,
        )

        save(figure, run, "block-granularity.svg", "virtio-mem block granularity")

        for name, label in ARMS:
            run.require(
                len(taken[name]["rounds"]) >= ROUNDS,
                f"{label} drove {len(taken[name]['rounds'])} grow round trips",
            )
            run.require(
                taken[name]["refused_actuations"] == 0,
                f"the device refused none of the resizes the daemon asked of it under "
                f"{label}",
            )
            run.require(
                taken[name]["round_trip_ms"]["median"] <= SANE,
                f"under {label} the median grow round trip is "
                f"{taken[name]['round_trip_ms']['median'] / 1000:.3f}s, which is "
                f"still a latency and not a hang",
            )

        for name, label in ARMS:
            run.note(
                f"under {label} the idle guest gave back "
                f"{taken[name]['device_returned_bytes'] / MIB:.0f}MiB of the "
                f"{(taken[name]['device_peak_bytes'] - 1792 * MIB) / MIB:.0f}MiB it "
                f"had taken"
            )
  '';
}
