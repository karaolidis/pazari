{
  description = "what MAP_NORESERVE does to Committed_AS, and what the brake sees either way";

  timeout = 420;

  daemon.policy = {
    host_floor = "1GiB";
    grant_min_interval = "2s";
  };

  guests =
    let
      arm = reserve: {
        daemon.ceiling = "1792MiB";

        module = { config, ... }: {
          microvm.hotplugMem = 512;

          environment.systemPackages = [ config.services.pazari-agent.package ];

          services.pazari-agent.harness.workload = {
            arguments = [
              "--interval"
              "250ms"
              "--arena"
              "1GiB"
            ]
            ++ reserve;

            phases = [
              "hold=5s"
              "grow=768MiB,rate=128MiB/s"
              "hold=15s"
              "release=512MiB"
              "hold=30s"
            ];
          };
        };
      };
    in
    {
      reserved = arm [ ];
      noreserve = arm [ "--no-reserve" ];
    };

  assertions = ''
    from pazari_harness.measure import bars, record, save
    from pazari_harness.units import GIB, MIB

    import matplotlib.pyplot as plt

    ARMS = [("reserved", "a committed arena"), ("noreserve", "MAP_NORESERVE")]

    STAGES = [("mapped", None), ("grown", 1), ("released", 3)]

    READINGS = [
        ("Committed_AS", "committed_as_bytes"),
        ("workload memory.current", "memory_current_bytes"),
        ("the workload's frontier", "frontier_bytes"),
    ]

    ARENA = GIB

    PATIENCE = 240

    SEPARATED = 768 * MIB

    TOUCHED = 512 * MIB

    SLOP = 256 * MIB

    OVERSIZED = "8GiB"

    def reading(run, guest, stage):
        _, meminfo = guest.shell(
            "grep -E '^(Committed_AS|CommitLimit|MemTotal):' /proc/meminfo",
            f"{guest.name}'s commit accounting was read once it had {stage}",
        )
        _, current = guest.shell(
            "cat /sys/fs/cgroup/workload.slice/memory.current",
            f"{guest.name}'s workload cgroup was read once it had {stage}",
        )

        held = {
            line.split(":")[0]: 1024 * int(line.split()[1])
            for line in meminfo.strip().split("\n")
        }

        return {
            "committed_as_bytes": held["Committed_AS"],
            "commit_limit_bytes": held["CommitLimit"],
            "mem_total_bytes": held["MemTotal"],
            "memory_current_bytes": int(current.strip()),
            "frontier_bytes": max(
                (event["frontier"] for event in guest.workload.progress()), default=0
            ),
        }

    def probe(run, guest):
        taken = {}

        for name, extra in [("reserved", ""), ("noreserve", "--no-reserve ")]:
            code, output = guest.shell(
                f"pazari-stress {extra}--arena {OVERSIZED} hold=1s",
                f"{guest.name} tried to map {OVERSIZED} as {name}",
                expect=None,
            )

            taken[name] = {"exit": code, "output": output.strip()[-200:]}

        return taken

    def follow(run):
        taken = {name: {} for name, _ in ARMS}

        for stage, phase in STAGES:
            for name, _ in ARMS:
                guest = run.guests[name]

                if phase is not None:
                    run.wait_for(
                        lambda guest=guest, phase=phase: guest.workload.of(
                            "completed", index=phase
                        ),
                        PATIENCE,
                        f"{name}'s workload has {stage} what its plan asked for",
                        when=lambda events: events[0]["at"],
                    )

                taken[name][stage] = reading(run, guest, stage)

        return taken

    def check(run):
        taken = follow(run)

        for name, _ in ARMS:
            record(run, name, taken[name])

        record(run, "oversized_arena", probe(run, run.guests["noreserve"]))

        figure, axes = plt.subplots(
            1, len(ARMS), figsize=(13, 4.8), dpi=100, sharey=True
        )

        for axis, (name, label) in zip(axes, ARMS, strict=True):
            bars(
                axis,
                [stage for stage, _ in STAGES],
                [
                    (
                        title,
                        [taken[name][stage][key] for stage, _ in STAGES],
                    )
                    for title, key in READINGS
                ],
                label,
                "where the run had got to",
                "bytes (GiB)",
                scale=GIB,
            )

        save(figure, run, "noreserve.svg", "commit accounting against the brake")

        run.require(
            taken["reserved"]["mapped"]["committed_as_bytes"]
            - taken["noreserve"]["mapped"]["committed_as_bytes"]
            >= SEPARATED,
            f"mapping the {ARENA / GIB:.0f}GiB arena without MAP_NORESERVE charged "
            f"Committed_AS for it, and mapping it with MAP_NORESERVE did not",
        )

        for name, label in ARMS:
            drift = abs(
                taken[name]["grown"]["committed_as_bytes"]
                - taken[name]["mapped"]["committed_as_bytes"]
            )

            run.require(
                drift <= SLOP,
                f"under {label}, Committed_AS moved {drift / MIB:.0f}MiB while the "
                f"workload was touching the arena: commit is charged to the mapping, "
                f"not to the pages",
            )
            run.require(
                taken[name]["grown"]["memory_current_bytes"]
                - taken[name]["mapped"]["memory_current_bytes"]
                >= TOUCHED,
                f"under {label}, the cgroup the agent brakes followed the frontier",
            )

        apart = abs(
            taken["reserved"]["grown"]["memory_current_bytes"]
            - taken["noreserve"]["grown"]["memory_current_bytes"]
        )

        run.require(
            apart <= SLOP,
            f"the two arms differ by {apart / MIB:.0f}MiB in what the agent can see, "
            f"so MAP_NORESERVE is invisible to the brake",
        )
  '';
}
