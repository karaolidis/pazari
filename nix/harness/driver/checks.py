"""The assertion vocabulary a scenario's own `check` is written in."""

from .units import MIB

BLOCK = 128 * MIB

PATIENCE = 180

COLLAPSED = 0.35
RECOVERED = 0.70
CRAWLING = 0.15


def paced(guest, index):
    for started in guest.workload.of("started"):
        plan = started["plan"]

        if index is not None and index < len(plan) and plan[index].get("rate_bps"):
            return float(plan[index]["rate_bps"])

    return max(
        (event["rate_bps"] for event in guest.workload.progress(index)), default=0.0
    )


def intervals(guest, index, share, faster, since=None):
    bar = share * paced(guest, index)

    return [
        event
        for event in guest.workload.progress(index)
        if (event["rate_bps"] > bar if faster else event["rate_bps"] < bar)
        and (since is None or event["at"] >= since)
    ]


def collapsed(run, guest, index, reason, share=COLLAPSED, timeout=PATIENCE):
    return run.wait_for(
        lambda: intervals(guest, index, share, faster=False),
        timeout,
        reason,
        when=lambda found: found[0]["at"],
    )


def recovered(run, guest, index, reason, since=None, share=RECOVERED, timeout=PATIENCE):
    return run.wait_for(
        lambda: intervals(guest, index, share, faster=True, since=since),
        timeout,
        reason,
        when=lambda found: found[0]["at"],
    )


def grew(run, guest, boot):
    guest.agent.rose(
        "pazari_agent_throttle_wakes_total",
        timeout=PATIENCE,
        reason=f"{guest.name}'s throttle counters woke the agent",
    )
    guest.agent.rose(
        "pazari_agent_grant_requests_total",
        timeout=PATIENCE,
        reason=f"{guest.name} asked the host for more memory",
    )
    run.daemon.rose(
        "pazarid_guest_messages_total",
        labels={"kind": "grant_request"},
        timeout=PATIENCE,
        reason="the daemon took the request",
    )
    run.daemon.rose(
        "pazarid_grants_total",
        timeout=PATIENCE,
        reason="the daemon granted the grow",
    )
    guest.agent.rose(
        "pazari_agent_grants_total",
        timeout=PATIENCE,
        reason=f"{guest.name} heard the grant",
    )

    return run.daemon.reaches(
        "pazarid_guest_size_bytes",
        lambda value: value >= boot + BLOCK,
        PATIENCE,
        f"{guest.name}'s memory device grew by at least one block",
        guest.labels,
    )


def followed(guest, floor):
    return guest.agent.reaches(
        "pazari_agent_total_mem_bytes",
        lambda value: value >= floor,
        PATIENCE,
        f"{guest.name}'s MemTotal followed the device",
    )


def steady(run, guest, reason="the engine settled into steady", since=None, held=6):
    start = run.clock() if since is None else since

    def rested():
        streak = []

        for at, value in guest.agent.series("pazari_agent_mode", {"mode": "steady"}):
            if at < start:
                continue

            streak = [*streak, at] if value == 1 else []

            if len(streak) >= held:
                return streak[0]

        return None

    return run.wait_for(rested, PATIENCE, reason, when=lambda at: at)
