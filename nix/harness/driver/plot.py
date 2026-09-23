"""One SVG per run: what the memory did, what the workload felt, what the engine was."""

import matplotlib

matplotlib.use("Agg")

import matplotlib.pyplot as plt

from .series import rises
from .units import GIB, MIB

MARKERS = [
    ("request", "agent", "pazari_agent_grant_requests_total"),
    ("grant", "agent", "pazari_agent_grants_total"),
    ("deny", "agent", "pazari_agent_denies_total"),
    ("offer", "agent", "pazari_agent_offers_total"),
    ("reclaim", "daemon", "pazarid_reclaim_requests_total"),
]

APART = 0.4

LANES = ["engine mode", "guest link"]


def draw(axis, points, label=None, scale=1.0, steps=True, gap=1.0):
    if not points:
        return

    times = []
    values = []

    for at, value in points:
        if times and gap is not None and at - times[-1] > gap:
            times.append(times[-1])
            values.append(float("nan"))

        times.append(at)
        values.append(value / scale)

    axis.plot(
        times,
        values,
        drawstyle="steps-post" if steps else "default",
        label=label,
    )


def spans(points, gap=1.0):
    out = []

    for at, state in points:
        if out and out[-1][2] == state and at - out[-1][1] <= gap:
            out[-1][1] = at
        else:
            out.append([at, at, state])

    return out


def link(run, guest):
    stale = dict(run.daemon.series("pazarid_guest_stale", guest.labels))

    return [
        (at, "down" if value != 1 else "stale" if stale.get(at) == 1 else "connected")
        for at, value in run.daemon.series("pazarid_guest_connected", guest.labels)
    ]


def lanes(axis, run, guest):
    rows = [guest.agent.winner("pazari_agent_mode", "mode"), link(run, guest)]
    colours = {}

    for row, points in enumerate(rows):
        taken = 0

        for start, end, state in spans(points):
            if state == "down" or end < run.origin:
                continue

            start = max(start, run.origin)
            label = None

            if state not in colours:
                colours[state] = f"C{taken}" if row == 0 else f"C{9 - taken}"
                label = state.replace("_", " ")
                taken += 1

            axis.barh(
                -row,
                end - start,
                left=start,
                height=0.6,
                color=colours[state],
                label=label,
            )

    axis.set_yticks([0, -1], LANES)
    axis.set_ylim(-1.7, 0.7)
    axis.legend(
        loc="lower left", bbox_to_anchor=(0, 1.02), ncol=8, frameon=False, fontsize=8
    )


def events(axis, run, guest):
    bottom, top = axis.get_ylim()
    seen = []

    for name, endpoint, metric in MARKERS:
        source = guest.agent if endpoint == "agent" else run.daemon

        for at in rises(source.totals(metric), APART):
            if at >= run.origin:
                seen.append((at, name))

    for index, (at, name) in enumerate(sorted(seen)):
        axis.axvline(at, linewidth=0.6, color="0.65", zorder=0)
        axis.annotate(
            name,
            xy=(at, top if index % 2 else bottom + (top - bottom) * 0.5),
            xytext=(-3, -4 if index % 2 else 0),
            textcoords="offset points",
            rotation=90,
            fontsize=7,
            color="0.35",
            ha="right",
            va="top" if index % 2 else "bottom",
        )


def plots(run):
    for name, guest in run.guests.items():
        path = run.directory / ("plot.svg" if len(run.guests) == 1 else f"{name}.svg")

        try:
            render(run, guest, path)
            run.note(f"{name}'s plot is {path.name}")
        except Exception as error:  # noqa: BLE001
            run.note(f"{name}'s plot could not be drawn: {error}")


def render(run, guest, path):
    figure, axes = plt.subplots(
        5,
        1,
        figsize=(13, 10),
        dpi=100,
        sharex=True,
        gridspec_kw={"height_ratios": [3, 1.2, 1, 1, 1], "hspace": 0.55},
    )

    memory, workload, available, pressure, swimlanes = axes

    draw(
        memory,
        run.daemon.series("pazarid_guest_size_bytes", guest.labels),
        "plugged",
        GIB,
    )
    draw(memory, guest.agent.series("pazari_agent_memory_max_bytes"), "memory.max", GIB)
    draw(
        memory, guest.agent.series("pazari_agent_memory_high_bytes"), "memory.high", GIB
    )
    draw(
        memory,
        guest.agent.series("pazari_agent_workload_usage_bytes"),
        "workload usage",
        GIB,
        steps=False,
    )
    draw(
        memory,
        [
            (at, value)
            for at, value in guest.agent.series("pazari_agent_ceiling_bytes")
            if value
        ],
        "ceiling",
        GIB,
    )

    memory.set_ylabel("memory (GiB)")
    memory.set_ylim(bottom=0)
    memory.legend(
        loc="lower left", bbox_to_anchor=(0, 1.02), ncol=5, frameon=False, fontsize=8
    )

    draw(
        workload,
        [(event["at"], event["rate_bps"]) for event in guest.workload.progress()],
        scale=MIB,
        gap=None,
    )
    workload.set_ylabel("workload fault rate\n(MiB/s, log)")
    workload.set_yscale("symlog", linthresh=1)
    workload.set_ylim(bottom=0)

    draw(
        available,
        run.daemon.series("pazarid_host_available_bytes"),
        scale=GIB,
        steps=False,
    )
    available.set_ylabel("host available\n(GiB)")

    draw(pressure, run.daemon.series("pazarid_host_psi"), steps=False)
    pressure.set_ylabel("host memory pressure\n(% of wall time)")
    pressure.set_ylim(0, max(pressure.get_ylim()[1], 1.0))

    memory.set_xlim(run.origin, max(run.clock(), run.origin + 1.0))
    events(memory, run, guest)

    lanes(swimlanes, run, guest)
    swimlanes.set_xlabel("seconds from the start of the run")

    for axis in axes[:4]:
        axis.grid(axis="y", linewidth=0.5)
        axis.set_axisbelow(True)

    figure.suptitle(
        f"{run.scenario.name} - {guest.name} - {run.stamp}", x=0.1, y=0.985, ha="left"
    )
    figure.subplots_adjust(left=0.1, right=0.985, top=0.9, bottom=0.06)
    figure.savefig(path, format="svg")
    plt.close(figure)
