"""One scenario, over the streams the machine running it offers."""

import importlib.util
import json
import os
import sys
import time
import traceback
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path

from .plot import plots
from .sampler import Sampler
from .series import gaps
from .streams import Jsonl, Tail
from .waiting import HarnessFailure, wait_for

SAMPLE_INTERVAL = 0.25

HELD_GAP = 4 * SAMPLE_INTERVAL

SOCKET_TIMEOUT = 60.0
BOOT_TIMEOUT = 420.0
CONNECT_TIMEOUT = 90.0
COMMAND_TIMEOUT = 60.0
UNIT_TIMEOUT = 300.0


@dataclass(frozen=True)
class Scenario:
    """One entry of the manifest the flake built."""

    name: str
    description: str
    timeout: float
    breaches: bool
    daemon: dict
    guests: dict
    assertions: str

    @classmethod
    def of(cls, entry):
        return cls(**entry)

    def ports(self):
        return {"daemon": self.daemon["port"]} | {
            name: spec["port"] for name, spec in self.guests.items()
        }

    def check(self):
        spec = importlib.util.spec_from_file_location(
            f"{self.name}_assertions", self.assertions
        )
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)

        return module.check


class Source:
    """One metrics endpoint and the waits a check may put on it."""

    def __init__(self, run, view):
        self._run = run
        self.view = view

    def series(self, name, labels=None):
        return self.view.series(name, labels)

    def totals(self, name):
        return self.view.totals(name)

    def winner(self, name, key):
        return self.view.winner(name, key)

    def peak(self, name, labels=None):
        return self.view.peak(name, labels)

    def gauge(self, name, labels=None):
        return self.view.latest(name, labels)

    def counter(self, name, labels=None):
        return self.view.latest(name, labels) or 0.0

    def reaches(self, name, predicate, timeout, reason, labels=None, since=None):
        start = self._run.clock() if since is None else since

        def settled():
            for at, value in self.view.series(name, labels):
                if at >= start and predicate(value):
                    return at, value

            return None

        return self._run.wait_for(
            settled, timeout, reason, when=lambda found: found[0]
        )[1]

    def rose(
        self, name, floor=None, labels=None, timeout=60.0, reason=None, since=None
    ):
        if floor is None:
            floor = self.view.first(name, self._run.origin, labels) + 1

        return self.reaches(
            name,
            lambda value: value >= floor,
            timeout,
            reason or f"{name} reached {floor:g}",
            labels,
            self._run.origin if since is None else since,
        )

    def held(self, name, predicate, over, labels=None, since=None):
        start = self._run.origin if since is None else since
        seen = [
            (at, value) for at, value in self.view.series(name, labels) if at >= start
        ]

        if not seen or seen[-1][0] - seen[0][0] < over:
            return False

        return max(gaps([at for at, _ in seen]), default=0.0) <= HELD_GAP and all(
            predicate(value) for _, value in seen
        )

    def answering(self, timeout, reason, ok=True, since=None):
        return self._run.wait_for(
            lambda: self.view.spoke(ok, since),
            timeout,
            reason,
            when=lambda found: found[0],
        )[0]


class Guest:
    """One guest the run watches and the run's hand inside it."""

    def __init__(self, run, name, spec, share, console):
        self._run = run
        self._issued = 0

        self.name = name
        self.spec = spec
        self.labels = {"guest": name}
        self.share = Path(share)
        self.control = self.share / spec["control"]

        self.agent = Source(run, run.sampler.view(name))
        self.workload = Jsonl(self.share / spec["log"], run.clock)
        self.console = Tail(console)

    def poll(self):
        self.workload.poll()
        self.console.poll()

    def release(self):
        (self.share / self.spec["gate"]).touch()

    def halt(self, reason=None):
        return self._run.halt(self, reason or f"{self.name}'s hypervisor was stopped")

    def shell(self, command, reason, timeout=COMMAND_TIMEOUT, expect=0):
        deadline = self._run.clock() + timeout

        def left():
            return max(0.0, deadline - self._run.clock())

        if not self._issued:
            self._run.wait_for(
                self.control.is_dir, left(), f"{self.name}'s courier is listening"
            )

        self._issued += 1
        issued = self._run.clock()

        stem = self.control / f"{self._issued:03d}"
        done = stem.with_suffix(".done")
        draft = stem.with_suffix(".part")

        draft.write_text(command + "\n", encoding="utf-8")
        os.replace(draft, stem.with_suffix(".cmd"))

        self._run.wait_for(done.exists, left(), f"{self.name}: {reason}")
        self._run.seen = issued

        answer = done.read_text(encoding="utf-8").strip()

        try:
            code = int(answer)
        except ValueError as malformed:
            raise HarnessFailure(
                f"{self.name}: the courier's answer to {command!r} was not an "
                f"exit code: {answer!r}"
            ) from malformed

        output = stem.with_suffix(".out").read_text(encoding="utf-8")

        if expect is not None and code != expect:
            raise HarnessFailure(
                f"{self.name}: {command!r} answered {code}, not {expect}: {output.strip()}"
            )

        return code, output


class Run:
    """Every stream a scenario's assertions get to look at."""

    def __init__(self, scenario, directory, shares, consoles):
        self.scenario = scenario
        self.started = time.monotonic()
        self.stamp = moment()

        self.directory = Path(directory)
        self.logs = self.directory / "logs"
        self.logs.mkdir(parents=True, exist_ok=True)

        self.sampler = Sampler(
            {
                name: f"http://127.0.0.1:{port}/metrics"
                for name, port in scenario.ports().items()
            },
            self.directory / "samples.jsonl",
            SAMPLE_INTERVAL,
            self.clock,
        )

        self.daemon = Source(self, self.sampler.view("daemon"))
        self.daemon_log = Tail(self.logs / "pazarid.log")

        self.guests = {
            name: Guest(self, name, spec, shares[name], consoles[name])
            for name, spec in scenario.guests.items()
        }

        self.alive = {}
        self.seen = 0.0
        self.origin = 0.0

        self._events = (self.directory / "events.jsonl").open("w", encoding="utf-8")

    @property
    def guest(self):
        if len(self.guests) != 1:
            raise HarnessFailure(
                f"{self.scenario.name} has {len(self.guests)} guests, "
                "so its check must name the one it means"
            )

        return next(iter(self.guests.values()))

    def clock(self):
        return time.monotonic() - self.started

    def note(self, message, at=None, **fields):
        at = self.clock() if at is None else at

        self._events.write(
            json.dumps(dict(fields, at=round(at, 3), message=message)) + "\n"
        )
        self._events.flush()

        print(f"  [{at:7.2f}s] {message}", flush=True)

    def poll(self):
        self.daemon_log.poll()

        for guest in self.guests.values():
            guest.poll()

    def guard(self):
        for check in list(self.alive.values()):
            check()

        if self.clock() > self.scenario.timeout:
            raise HarnessFailure(
                f"the scenario outlived its budget of {self.scenario.timeout:g}s"
            )

        if self.scenario.breaches:
            return

        for guest in self.guests.values():
            if guest.agent.counter("pazari_agent_max_events") > 0:
                raise HarnessFailure(
                    f"{guest.name} reached memory.max: the brake never caught the "
                    "workload, and the kernel killed it instead"
                )

    def wait_for(self, predicate, timeout, reason, when=None):
        found = wait_for(predicate, timeout, reason, poll=self.poll, guard=self.guard)

        self.seen = self.clock() if when is None else when(found)
        self.note(reason, at=self.seen)

        return found

    def require(self, condition, message):
        if not condition:
            raise HarnessFailure(message)

        self.note(message)

    def bring_up(self):
        pass

    def halt(self, guest, reason):
        raise NotImplementedError

    def begin(self):
        self.note(f"the run directory is {self.directory}")
        self.sampler.start()
        self.bring_up()

        self.daemon.answering(SOCKET_TIMEOUT, "the daemon is answering scrapes")

        for guest in self.guests.values():
            guest.agent.answering(
                BOOT_TIMEOUT, f"{guest.name} booted and its agent is up"
            )
            self.daemon.reaches(
                "pazarid_guest_connected",
                lambda value: value == 1,
                CONNECT_TIMEOUT,
                f"{guest.name} is connected and past the handshake",
                guest.labels,
            )

        for guest in self.guests.values():
            guest.release()

        self.note("the workloads are released")

        starts = [
            self.wait_for(
                lambda guest=guest: guest.workload.of("started"),
                CONNECT_TIMEOUT,
                f"{guest.name}'s workload is running",
                when=lambda events: events[0]["at"],
            )[0]["at"]
            for guest in self.guests.values()
        ]

        self.origin = min(starts)

    def stop(self):
        self.poll()
        self.sampler.stop()

    def record(self, result, detail):
        (self.directory / "result.json").write_text(
            json.dumps(
                {
                    "scenario": self.scenario.name,
                    "description": self.scenario.description,
                    "result": result,
                    "detail": detail,
                    "seconds": round(self.clock(), 2),
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

        self._events.close()


def moment():
    return datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S")


def perform(run, entry, check):
    (run.directory / "scenario.json").write_text(
        json.dumps(entry, indent=2) + "\n", encoding="utf-8"
    )

    result, detail = "passed", None

    try:
        run.begin()
        check(run)
        run.note("every assertion held")
    except HarnessFailure as error:
        result, detail = "failed", str(error)
        run.note(f"the scenario failed: {error}")
    except KeyboardInterrupt:
        result, detail = "interrupted", "the operator stopped the run"
        run.note("interrupted")
    except Exception:  # noqa: BLE001
        result, detail = "failed", f"the driver itself broke:\n{traceback.format_exc()}"
        run.note("the driver itself broke, result.json carries the traceback")
    finally:
        run.stop()

    elapsed = run.clock()

    plots(run)
    run.record(result, detail)

    return run, result, detail, elapsed


def drive(name, description, execute):
    print(f"running {name}: {description}")

    try:
        run, result, detail, elapsed = execute()
    except HarnessFailure as error:
        print(f"\n{error}", file=sys.stderr)

        return 1

    print(f"\n{name}: {result} in {elapsed:.1f}s")

    if detail is not None:
        print(detail, file=sys.stderr)

    print(run.directory)

    return 0 if result == "passed" else 1
