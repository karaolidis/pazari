"""What a measurement run takes off the streams and the figures it draws."""

import json
import statistics
from itertools import pairwise

import matplotlib

matplotlib.use("Agg")

import matplotlib.pyplot as plt

from .units import MIB
from .waiting import HarnessFailure, wait_for

MEASUREMENTS = "measurements.json"

MARGIN = 120


def steps(points):
    return [at for (_, before), (at, value) in pairwise(points) if value > before]


def deltas(points):
    return [
        later - earlier
        for (_, earlier), (_, later) in pairwise(points)
        if later > earlier
    ]


def after(times, start):
    return next((at for at in times if at > start), None)


def before(times, end):
    return next((at for at in reversed(times) if at <= end), None)


def settled(run, predicate, timeout, reason):
    try:
        found = wait_for(predicate, timeout, reason, poll=run.poll, guard=run.guard)
    except HarnessFailure:
        run.note(f"{reason}: not within {timeout:g}s")

        return None

    run.note(reason)

    return found


def cadence(source):
    times = [record["at"] for record in source.view.records() if record["ok"]]

    return [later - earlier for earlier, later in pairwise(times)]


def quantiles(values):
    if not values:
        return None

    ordered = sorted(values)

    return {
        "count": len(ordered),
        "min": ordered[0],
        "median": statistics.median(ordered),
        "p95": ordered[min(len(ordered) - 1, round(0.95 * (len(ordered) - 1)))],
        "max": ordered[-1],
        "mean": statistics.fmean(ordered),
    }


def record(run, name, value):
    path = run.directory / MEASUREMENTS
    held = json.loads(path.read_text(encoding="utf-8")) if path.exists() else {}
    held[name] = value

    path.write_text(json.dumps(held, indent=2) + "\n", encoding="utf-8")

    run.note(f"{name}: {json.dumps(value)}")

    return value


def cycles(guest):
    requests = steps(guest.agent.totals("pazari_agent_grant_requests_total"))
    grants = steps(guest.agent.totals("pazari_agent_grants_total"))
    denies = steps(guest.agent.totals("pazari_agent_denies_total"))
    plugs = steps(guest.agent.series("pazari_agent_total_mem_bytes"))

    out = []

    for index, granted in enumerate(grants):
        asked = before(requests, granted)
        plugged = after(plugs, granted)

        if asked is None or plugged is None:
            continue

        if any(asked < refused < granted for refused in denies):
            continue

        out.append(
            {
                "index": index,
                "requested": asked,
                "granted": granted,
                "plugged": plugged,
                "negotiation": granted - asked,
                "actuation": plugged - granted,
                "round_trip": plugged - asked,
            }
        )

    return out


def refused(guest):
    return next(iter(steps(guest.agent.totals("pazari_agent_exhaustions_total"))), None)


def band(guest, since):
    top = dict(guest.agent.series("pazari_agent_memory_max_bytes"))

    return next(
        (
            top[at] - high
            for at, high in guest.agent.series("pazari_agent_memory_high_bytes")
            if at >= since and at in top
        ),
        None,
    )


def crossing(guest, since):
    high = dict(guest.agent.series("pazari_agent_memory_high_bytes"))

    return next(
        (
            at
            for at, usage in guest.agent.series("pazari_agent_workload_usage_bytes")
            if at >= since and at in high and usage >= high[at]
        ),
        None,
    )


def overage(guest, since):
    high = dict(guest.agent.series("pazari_agent_memory_high_bytes"))

    return [
        (at - since, usage - high[at])
        for at, usage in guest.agent.series("pazari_agent_workload_usage_bytes")
        if at >= since and at in high
    ]


def slope(points, window):
    held = [(at, value) for at, value in points if at <= window]

    if len(held) < 2 or held[-1][0] <= held[0][0]:
        return None

    return (held[-1][1] - held[0][1]) / (held[-1][0] - held[0][0])


def killed(guest):
    return next(iter(steps(guest.agent.totals("pazari_agent_max_events"))), None)


def crawl(run, arms, window):
    for arm in arms:
        arm["guest"] = run.guests[arm["name"]]
        arm["window"] = window

    def done():
        for arm in arms:
            if killed(arm["guest"]) is not None:
                continue

            since = refused(arm["guest"])

            if since is None or run.clock() - since < window:
                return None

        return True

    settled(run, done, window + MARGIN, "every arm ran its course")

    for arm in arms:
        guest = arm["guest"]
        since = refused(guest)
        died = killed(guest)

        if since is None:
            raise HarnessFailure(
                f"{arm['name']} was never refused a grow, so it never ran with the "
                "band its posture was meant to leave it, and there is no crawl to "
                "measure"
            )

        crossed = crossing(guest, since)
        start = since if crossed is None else crossed

        arm["overage"] = [
            (at, value)
            for at, value in overage(guest, start)
            if died is None or at <= died - start
        ]
        arm["exhausted"] = None if died is None else died - since
        arm["measured_guard"] = band(guest, since)
        arm["peak"] = max((value for _, value in arm["overage"]), default=0)
        arm["through_guard"] = (
            None if died is None or crossed is None else died - crossed
        )
        arm["crawl_bps"] = (
            arm["peak"] / window
            if arm["through_guard"] is None
            else arm["guard"] / arm["through_guard"]
        )

    return arms


def frame(axes, title, x, y, log=False):
    axes.set_title(title, fontsize=10, loc="left")
    axes.set_xlabel(x, fontsize=9)
    axes.set_ylabel(y, fontsize=9)

    if log:
        axes.set_yscale("log")

    axes.grid(linewidth=0.5)
    axes.set_axisbelow(True)


def histogram(axes, values, title, x, unit, bins=20):
    axes.hist(values, bins=bins, color="C0", edgecolor="white", linewidth=0.5)

    shape = quantiles(values)

    for name, at, colour in [
        ("median", shape["median"], "C3"),
        ("p95", shape["p95"], "C1"),
    ]:
        axes.axvline(at, color=colour, linewidth=1.2, linestyle="--")
        axes.annotate(
            f"{name} {at:.0f} {unit}",
            xy=(at, axes.get_ylim()[1]),
            xytext=(3, -3),
            textcoords="offset points",
            fontsize=8,
            color=colour,
            va="top",
        )

    frame(axes, title, x, f"cycles per bin (of {shape['count']})")


def cdf(axes, samples, title, unit):
    for label, values in samples:
        if not values:
            continue

        ordered = sorted(values)
        shares = [(index + 1) / len(ordered) for index in range(len(ordered))]

        axes.plot(ordered, shares, drawstyle="steps-post", label=label)

    axes.set_ylim(0, 1.02)
    axes.legend(fontsize=8, frameon=False, loc="lower right")

    frame(axes, title, unit, "share of cycles at or under")


def bars(axes, groups, series, title, x, y, scale=1.0):
    width = 0.8 / max(len(series), 1)
    middle = (len(series) - 1) / 2

    for index, (label, values) in enumerate(series):
        axes.bar(
            [place + (index - middle) * width for place in range(len(groups))],
            [value / scale for value in values],
            width=width,
            label=label,
        )

    axes.set_xticks(range(len(groups)), groups, fontsize=8)
    axes.legend(fontsize=8, frameon=False)

    frame(axes, title, x, y)


def save(figure, run, path, title):
    figure.suptitle(f"{title} - {run.scenario.name} - {run.stamp}", x=0.02, ha="left")
    figure.tight_layout(rect=(0, 0, 1, 0.96))
    figure.savefig(run.directory / path, format="svg")
    plt.close(figure)

    run.note(f"the figure is {path}")


def rtt_figure(run, path, rounds):
    figure, axes = plt.subplots(1, 3, figsize=(15, 4.5), dpi=100)
    spread, legs, drift = axes

    total = [1000 * cycle["round_trip"] for cycle in rounds]

    histogram(spread, total, "the grow round trip", "request to MemTotal (ms)", "ms")

    cdf(
        legs,
        [
            ("request to grant", [1000 * cycle["negotiation"] for cycle in rounds]),
            ("grant to MemTotal", [1000 * cycle["actuation"] for cycle in rounds]),
            ("request to MemTotal", total),
        ],
        "where the round trip went",
        "milliseconds",
    )

    drift.plot(
        [cycle["index"] for cycle in rounds],
        total,
        marker="o",
        markersize=3,
        linewidth=0.8,
    )
    frame(drift, "round trip over the run", "grow cycle", "milliseconds")

    save(figure, run, path, "hotplug round trip")


def crawl_figure(run, path, arms):
    figure, axes = plt.subplots(1, 2, figsize=(13, 4.8), dpi=100)
    profile, sweep = axes

    for arm in arms:
        points = arm["overage"]

        if points:
            profile.plot(
                [at for at, _ in points],
                [value / MIB for _, value in points],
                label=arm["label"],
                linewidth=1.0,
            )

    frame(
        profile,
        "the climb to memory.max",
        "seconds since the workload first reached memory.high",
        "workload usage over memory.high (MiB)",
    )
    profile.legend(fontsize=8, frameon=False, loc="lower right")

    swept = sorted(
        (arm for arm in arms if arm["guard"] > 0), key=lambda arm: arm["guard"]
    )

    ran_out = [arm for arm in swept if arm["exhausted"] is not None]
    survived = [arm for arm in swept if arm["exhausted"] is None]

    if ran_out:
        sweep.plot(
            [arm["guard"] / MIB for arm in ran_out],
            [arm["exhausted"] for arm in ran_out],
            marker="o",
            linewidth=1.0,
            color="C0",
            label="reached memory.max",
        )

    if survived:
        sweep.plot(
            [arm["guard"] / MIB for arm in survived],
            [arm["window"] for arm in survived],
            marker="^",
            color="C7",
            linestyle="none",
            label="still alive when the window closed",
        )

    sweep.set_xscale("log", base=2)
    sweep.set_yscale("log")
    sweep.legend(fontsize=8, frameon=False, loc="upper left")

    frame(
        sweep,
        "how long a brake of each width held",
        "burst_reserve (MiB)",
        "seconds from the refusal to memory.max",
    )

    save(figure, run, path, "brake hold")
