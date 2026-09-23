{
  description = "how long an open band of each width holds a swap-denied burst under memory.max";

  timeout = 900;

  sample = 0.05;

  breaches = true;

  daemon.policy = {
    host_floor = "2GiB";
    grant_min_interval = "2s";
  };

  guests =
    let
      arm = guard: {
        daemon.ceiling = "1792MiB";

        module = { lib, ... }: {
          microvm.hotplugMem = 512;

          services.pazari-agent = {
            settings.tick_interval = lib.mkForce "100ms";

            harness = {
              engine = {
                burst_reserve = "512MiB";

                exhaustion = {
                  kind = "stall";
                  timeout = "900s";
                };
              }
              // guard;

              workload.phases = [
                "grow=400MiB,rate=200MiB/s"
                "grow=1200MiB"
                "hold=800s"
              ];
            };
          };
        };
      };
    in
    {
      guard16 = arm { open_guard = "16MiB"; };
      guard64 = arm { open_guard = "64MiB"; };
      guard256 = arm { open_guard = "256MiB"; };
      collapsed = arm { exhaustion.kind = "kill"; };
    };

  assertions = ''
    from pazari_harness.measure import crawl, crawl_figure, record
    from pazari_harness.units import MIB

    WINDOW = 360

    CLAIM = 90

    PROMPT = 5

    ARMS = [
        {"name": "collapsed", "guard": 0, "label": "no guard, memory.high = memory.max"},
        {"name": "guard16", "guard": 16 * MIB, "label": "open_guard = 16MiB"},
        {"name": "guard64", "guard": 64 * MIB, "label": "open_guard = 64MiB"},
        {"name": "guard256", "guard": 256 * MIB, "label": "open_guard = 256MiB"},
    ]

    def check(run):
        arms = crawl(run, ARMS, WINDOW)

        for arm in arms:
            record(
                run,
                arm["name"],
                {
                    "open_guard_bytes": arm["guard"],
                    "measured_band_bytes": arm["measured_guard"],
                    "seconds_to_memory_max": arm["exhausted"],
                    "seconds_through_the_guard": arm["through_guard"],
                    "peak_over_memory_high_bytes": arm["peak"],
                    "observed_for_seconds": WINDOW,
                    "crawl_bytes_per_second": arm["crawl_bps"],
                },
            )

        crawl_figure(run, "brake-crawl.svg", arms, claim=CLAIM)

        held = {arm["name"]: arm["exhausted"] for arm in arms}

        run.require(
            held["collapsed"] is not None and held["collapsed"] <= PROMPT,
            f"a collapsed band let the burst reach memory.max in "
            f"{held['collapsed']}s, which is no brake at all",
        )

        for name in ["guard16", "guard64", "guard256"]:
            run.require(
                held[name] is None or held[name] > held["collapsed"],
                f"{name} held the same burst longer than a collapsed band did",
            )

        run.require(
            held["guard64"] is None or held["guard64"] >= CLAIM,
            f"a 64MiB guard held the burst for "
            f"{WINDOW if held['guard64'] is None else held['guard64']}s, against the "
            f"{CLAIM}s the sizing guidance claims",
        )

        for smaller, larger in [("guard16", "guard64"), ("guard64", "guard256")]:
            if held[smaller] is not None and held[larger] is not None:
                run.require(
                    held[larger] >= held[smaller],
                    f"{larger} held longer than {smaller}, so the guard is what buys "
                    f"the time",
                )
  '';
}
