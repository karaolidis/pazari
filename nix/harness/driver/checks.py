"""The assertion vocabulary a scenario's own `check` is written in."""

from .units import MIB

BLOCK = 128 * MIB

COLLAPSED = 0.35
RECOVERED = 0.70
CRAWLING = 0.15


def paced(run, index):
    for started in run.workload.of("started"):
        plan = started["plan"]

        if index is not None and index < len(plan) and plan[index].get("rate_bps"):
            return float(plan[index]["rate_bps"])

    return max(
        (event["rate_bps"] for event in run.workload.progress(index)), default=0.0
    )


def intervals(run, index, share, faster, since=None):
    bar = share * paced(run, index)

    return [
        event
        for event in run.workload.progress(index)
        if (event["rate_bps"] > bar if faster else event["rate_bps"] < bar)
        and (since is None or event["at"] >= since)
    ]


def collapsed(run, index, reason, share=COLLAPSED, timeout=120):
    return run.wait_for(
        lambda: intervals(run, index, share, faster=False),
        timeout,
        reason,
        when=lambda found: found[0]["at"],
    )


def recovered(run, index, reason, since=None, share=RECOVERED, timeout=120):
    return run.wait_for(
        lambda: intervals(run, index, share, faster=True, since=since),
        timeout,
        reason,
        when=lambda found: found[0]["at"],
    )


def grew(run, guest, boot):
    run.counter_rose(
        "agent",
        "pazari_agent_throttle_wakes_total",
        timeout=120,
        reason="the workload's throttle counters woke the agent",
    )
    run.counter_rose(
        "agent",
        "pazari_agent_grant_requests_total",
        timeout=60,
        reason="the agent asked the host for more memory",
    )
    run.counter_rose(
        "daemon",
        "pazarid_guest_messages_total",
        labels={"kind": "grant_request"},
        timeout=60,
        reason="the daemon took the request",
    )
    run.counter_rose(
        "daemon",
        "pazarid_grants_total",
        timeout=90,
        reason="the daemon granted the grow",
    )
    run.counter_rose(
        "agent",
        "pazari_agent_grants_total",
        timeout=60,
        reason="the agent heard the grant",
    )

    return run.gauge_reaches(
        "daemon",
        "pazarid_guest_size_bytes",
        lambda value: value >= boot + BLOCK,
        90,
        "the guest's memory device grew by at least one block",
        guest,
    )


def followed(run, floor):
    return run.gauge_reaches(
        "agent",
        "pazari_agent_plugged_bytes",
        lambda value: value >= floor,
        90,
        "the guest's MemTotal followed the device",
    )


def steady(run, reason="the engine settled into steady", since=None, held=6):
    start = run.clock() if since is None else since

    def rested():
        streak = []

        for at, value in run.agent.series("pazari_agent_mode", {"mode": "steady"}):
            if at < start:
                continue

            streak = [*streak, at] if value == 1 else []

            if len(streak) >= held:
                return streak[0]

        return None

    return run.wait_for(rested, 150, reason, when=lambda at: at)
