"""Synchronisation on what a run can observe, never on a bare delay."""

import time

INTERVAL = 0.1


class HarnessFailure(Exception):
    """A scenario did not hold. The run directory carries the evidence."""


def wait_for(predicate, timeout, reason, poll=None, guard=None, interval=INTERVAL):
    """Polls `predicate` until it is truthy, and returns what it gave back."""

    deadline = time.monotonic() + timeout

    while True:
        if poll is not None:
            poll()

        found = predicate()

        if found:
            return found

        if guard is not None:
            guard()

        if time.monotonic() >= deadline:
            raise HarnessFailure(f"waited {timeout:g}s for {reason}, and it never came")

        time.sleep(interval)
