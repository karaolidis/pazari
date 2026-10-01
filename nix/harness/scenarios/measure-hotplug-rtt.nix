{
  description = "how long one grow round trip takes, and what the agent's tick costs it";

  timeout = 600;

  sample = 0.05;

  daemon.policy = {
    host_floor = "1GiB";
    grant_min_interval = "500ms";
  };

  guests =
    let
      arm = tick: {
        daemon.ceiling = "4608MiB";

        module = { lib, ... }: {
          services.pazari-agent = {
            settings.tick_interval = lib.mkForce tick;

            harness = {
              engine = {
                grow = {
                  kind = "exponential";
                  step = "128MiB";
                };
                settle_grace = "3s";
                deny_backoff = "1s";
              };

              workload.phases = [
                "grow=3584MiB,rate=24MiB/s"
                "hold=20s"
              ];
            };
          };
        };
      };
    in
    {
      tick500 = arm "500ms";
      tick100 = arm "100ms";
    };

  assertions = ''
    from pazari_harness.measure import (
        cadence,
        cdf,
        cycles,
        deltas,
        quantiles,
        record,
        rtt_figure,
        save,
    )

    import matplotlib.pyplot as plt

    ARMS = [("tick500", "tick_interval = 500ms"), ("tick100", "tick_interval = 100ms")]

    ROUNDS = 8

    PATIENCE = 540

    SANE = 30.0

    def measure(run, name):
        guest = run.guests[name]

        run.wait_for(
            lambda: guest.workload.of("done"),
            PATIENCE,
            f"{name}'s workload finished its plan",
            when=lambda events: events[0]["at"],
        )

        rounds = cycles(guest)

        record(
            run,
            name,
            {
                "rounds": len(rounds),
                "negotiation_ms": quantiles([1000 * cycle["negotiation"] for cycle in rounds]),
                "actuation_ms": quantiles([1000 * cycle["actuation"] for cycle in rounds]),
                "round_trip_ms": quantiles([1000 * cycle["round_trip"] for cycle in rounds]),
                "plug_step_bytes": quantiles(
                    deltas(guest.agent.series("pazari_agent_total_mem_bytes"))
                ),
                "grant_requests": guest.agent.counter("pazari_agent_grant_requests_total"),
                "grants": guest.agent.counter("pazari_agent_grants_total"),
                "denies_by_ceiling": guest.agent.counter(
                    "pazari_agent_denies_total", {"reason": "ceiling"}
                ),
                "scrape_interval_ms": quantiles([1000 * gap for gap in cadence(guest.agent)]),
            },
        )

        if len(rounds) >= 2:
            rtt_figure(run, f"hotplug-rtt-{name}.svg", rounds)

        return rounds

    def check(run):
        taken = {name: measure(run, name) for name, _ in ARMS}

        figure, axes = plt.subplots(1, 1, figsize=(7.5, 4.8), dpi=100)

        cdf(
            axes,
            [
                (label, [1000 * cycle["round_trip"] for cycle in taken[name]])
                for name, label in ARMS
            ],
            "the grow round trip under each agent tick",
            "request to MemTotal (milliseconds)",
        )

        save(figure, run, "hotplug-rtt.svg", "hotplug round trip against the tick")

        for name, label in ARMS:
            rounds = taken[name]

            run.require(
                len(rounds) >= ROUNDS,
                f"{label} drove {len(rounds)} grow round trips, enough to say "
                f"something about their spread",
            )

            shape = quantiles([cycle["round_trip"] for cycle in rounds])

            run.require(
                shape["median"] <= SANE,
                f"under {label} the median grow round trip is "
                f"{shape['median']:.3f}s, which is still a latency and not a hang",
            )
            run.require(
                shape["p95"] <= 2 * SANE,
                f"under {label} the 95th percentile grow round trip is "
                f"{shape['p95']:.3f}s",
            )
            run.require(
                run.guests[name].agent.counter("pazari_agent_max_events") == 0,
                f"under {label} the brake caught the workload through every one of "
                f"those cycles",
            )
  '';
}
