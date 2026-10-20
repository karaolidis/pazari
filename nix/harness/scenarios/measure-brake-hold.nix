{
  description = "how long the brake holds a swap-denied burst under memory.max, against reserve and allocation size";

  timeout = 900;

  sample = 0.05;

  breaches = true;

  daemon.settings.grant = {
    floor = "2GiB";
    min_interval = "2s";
  };

  guests =
    let
      arm =
        {
          reserve ? "512MiB",
          chunk ? "1MiB",
          exhaustion ? {
            kind = "stall";
            timeout = "900s";
          },
        }:
        {
          daemon.ceiling = "1792MiB";

          module = { ... }: {
            microvm.hotplugMem = 512;

            services.pazari-agent = {
              settings = {
                tick.interval = "100ms";
                workload.burst_reserve = reserve;
                exhaustion.mode = exhaustion;
              };

              harness = {
                workload.phases = [
                  "grow=400MiB,rate=200MiB/s"
                  "grow=1200MiB,chunk=${chunk}"
                  "hold=800s"
                ];
              };
            };
          };
        };
    in
    {
      reserve64 = arm { reserve = "64MiB"; };
      reserve256 = arm { reserve = "256MiB"; };
      reserve512 = arm { reserve = "512MiB"; };
      coarse = arm {
        reserve = "512MiB";
        chunk = "256MiB";
      };
      released = arm { exhaustion.kind = "kill"; };
    };

  assertions = ''
    from pazari_harness.measure import crawl, crawl_figure, record
    from pazari_harness.units import MIB

    WINDOW = 360

    CLAIM = 90

    PROMPT = 5

    ARMS = [
        {"name": "released", "guard": 0, "label": "brake released, memory.high = memory.max"},
        {"name": "reserve64", "guard": 64 * MIB, "label": "burst_reserve = 64MiB"},
        {"name": "reserve256", "guard": 256 * MIB, "label": "burst_reserve = 256MiB"},
        {"name": "reserve512", "guard": 512 * MIB, "label": "burst_reserve = 512MiB"},
        {"name": "coarse", "guard": 512 * MIB, "label": "burst_reserve = 512MiB, 256MiB faults"},
    ]

    def check(run):
        arms = crawl(run, ARMS, WINDOW)

        for arm in arms:
            record(
                run,
                arm["name"],
                {
                    "burst_reserve_bytes": arm["guard"],
                    "measured_band_bytes": arm["measured_guard"],
                    "seconds_to_memory_max": arm["exhausted"],
                    "seconds_through_the_band": arm["through_guard"],
                    "peak_over_memory_high_bytes": arm["peak"],
                    "observed_for_seconds": WINDOW,
                    "crawl_bytes_per_second": arm["crawl_bps"],
                },
            )

        crawl_figure(run, "brake-hold.svg", arms, claim=CLAIM)

        held = {arm["name"]: arm["exhausted"] for arm in arms}

        run.require(
            held["released"] is not None and held["released"] <= PROMPT,
            f"a released brake let the burst reach memory.max in "
            f"{held['released']}s, which is no brake at all",
        )

        for name in ["reserve64", "reserve256", "reserve512", "coarse"]:
            run.require(
                held[name] is None or held[name] > held["released"],
                f"{name} held the same burst no longer than a released brake did",
            )

        run.require(
            held["reserve512"] is None or held["reserve512"] >= CLAIM,
            f"a 512MiB burst reserve held the burst for "
            f"{WINDOW if held['reserve512'] is None else held['reserve512']}s, against the "
            f"{CLAIM}s the sizing guidance claims",
        )

        for smaller, larger in [("reserve64", "reserve256"), ("reserve256", "reserve512")]:
            if held[smaller] is not None and held[larger] is not None:
                run.require(
                    held[larger] >= held[smaller],
                    f"{larger} held longer than {smaller}, so the reserve is what buys "
                    f"the time",
                )

        run.require(
            held["coarse"] is None or held["reserve512"] is None or held["coarse"] <= held["reserve512"],
            f"faulting 256MiB at a time crossed the same reserve no faster than 1MiB "
            f"faults did, which the sizing guidance does not expect",
        )
  '';
}
