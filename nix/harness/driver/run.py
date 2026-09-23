"""One scenario, in one real microvm, from the run directory up."""

import importlib.util
import json
import socket
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path

from .process import Process
from .sampler import Sampler
from .streams import Jsonl, Tail
from .waiting import HarnessFailure, wait_for

PLACEHOLDER = "@QMP_SOCKET@"

SAMPLE_INTERVAL = 0.25

BOOT_TIMEOUT = 240.0
SOCKET_TIMEOUT = 60.0
CONNECT_TIMEOUT = 90.0

NODES = ("/dev/kvm", "/dev/vhost-vsock")


@dataclass(frozen=True)
class Scenario:
    """One entry of the manifest the flake built."""

    name: str
    description: str
    timeout: float
    guest: str
    runner: str
    settings: str
    assertions: str
    share: str
    log: str
    gate: str
    qmp: str
    ports: dict

    @classmethod
    def of(cls, name, entry):
        return cls(name=name, **entry)

    def check(self):
        spec = importlib.util.spec_from_file_location(
            f"{self.name}_assertions", self.assertions
        )
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)

        return module.check

    def preflight(self):
        faults = []

        for path in NODES:
            node = Path(path)

            if not node.exists():
                faults.append(f"{path} is missing: the test needs KVM and vhost-vsock")
            elif not node.stat().st_mode & 0o006:
                faults.append(f"{path} is not readable and writable by this user")

        for name, port in self.ports.items():
            with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
                probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)

                try:
                    probe.bind(("127.0.0.1", port))
                except OSError:
                    faults.append(
                        f"127.0.0.1:{port} is in use, and {name} metrics want it"
                    )

        if faults:
            raise HarnessFailure(
                "this host cannot run the test:\n  - " + "\n  - ".join(faults)
            )


class Run:
    """Every stream a scenario's assertions get to look at."""

    def __init__(self, scenario, root):
        self.scenario = scenario
        self.started = time.monotonic()
        self.stamp = datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S")

        self.directory = Path(root) / f"{scenario.name}-{self.stamp}"
        self.logs = self.directory / "logs"
        self.qmp = self.directory / scenario.qmp

        share = self.directory / scenario.share

        for made in (self.logs, share):
            made.mkdir(parents=True)

        self.gate = share / scenario.gate

        self.qemu = Process(
            "qemu",
            [f"{scenario.runner}/bin/microvm-run"],
            self.directory,
            self.logs / "qemu.log",
        )
        self.pazarid = None

        self.console = Tail(self.logs / "qemu.log")
        self.daemon_log = Tail(self.logs / "pazarid.log")
        self.workload = Jsonl(share / scenario.log, self.clock)

        self.sampler = Sampler(
            {
                name: f"http://127.0.0.1:{port}/metrics"
                for name, port in scenario.ports.items()
            },
            self.directory / "samples.jsonl",
            SAMPLE_INTERVAL,
            self.clock,
        )

        self.agent = self.sampler.view("agent")
        self.daemon = self.sampler.view("daemon")
        self.seen = 0.0
        self.origin = 0.0

        self._events = (self.directory / "events.jsonl").open("w", encoding="utf-8")

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
        self.workload.poll()
        self.console.poll()
        self.daemon_log.poll()

    def guard(self):
        self.qemu.require_alive()

        if self.pazarid is not None:
            self.pazarid.require_alive()

        if self.clock() > self.scenario.timeout:
            raise HarnessFailure(
                f"the scenario outlived its budget of {self.scenario.timeout:g}s"
            )

        if self.counter("agent", "pazari_agent_max_events") > 0:
            raise HarnessFailure(
                "the workload reached memory.max: the brake never caught it, "
                "and the kernel killed the workload instead"
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

    def view(self, source):
        return self.agent if source == "agent" else self.daemon

    def gauge(self, source, name, labels=None):
        return self.view(source).latest(name, labels)

    def counter(self, source, name, labels=None):
        return self.view(source).latest(name, labels) or 0.0

    def gauge_reaches(
        self, source, name, predicate, timeout, reason, labels=None, since=None
    ):
        start = self.clock() if since is None else since

        def settled():
            for at, value in self.view(source).series(name, labels):
                if at >= start and predicate(value):
                    return at, value

            return None

        return self.wait_for(settled, timeout, reason, when=lambda found: found[0])[1]

    def counter_rose(
        self,
        source,
        name,
        floor=None,
        labels=None,
        timeout=60.0,
        reason=None,
        since=None,
    ):
        view = self.view(source)

        if floor is None:
            floor = view.first(name, self.origin, labels) + 1

        return self.gauge_reaches(
            source,
            name,
            lambda value: value >= floor,
            timeout,
            reason or f"{name} reached {floor:g}",
            labels,
            self.origin if since is None else since,
        )

    def answering(self, source, timeout, reason, ok=True, since=None):
        return self.wait_for(
            lambda: self.view(source).spoke(ok, since),
            timeout,
            reason,
            when=lambda found: found[0],
        )[0]

    def start(self, pazarid):
        self.note(f"the run directory is {self.directory}")
        self.sampler.start()

        self.note("qemu is up", pid=self.qemu.start())
        self.wait_for(
            self.qmp.exists, SOCKET_TIMEOUT, "qemu opened the daemon's QMP socket"
        )

        config = self.directory / "pazarid.toml"
        config.write_text(
            Path(self.scenario.settings)
            .read_text(encoding="utf-8")
            .replace(PLACEHOLDER, str(self.qmp)),
            encoding="utf-8",
        )

        self.pazarid = Process(
            "pazarid",
            [pazarid, "--config", str(config)],
            self.directory,
            self.logs / "pazarid.log",
            {"RUST_LOG": "info"},
        )
        self.note("pazarid is up", pid=self.pazarid.start())

        self.answering("daemon", SOCKET_TIMEOUT, "the daemon is answering scrapes")
        self.answering("agent", BOOT_TIMEOUT, "the guest booted and the agent is up")

        self.gauge_reaches(
            "daemon",
            "pazarid_guest_connected",
            lambda value: value == 1,
            CONNECT_TIMEOUT,
            "the guest is connected and past the handshake",
            {"guest": self.scenario.guest},
        )

        self.gate.touch()
        self.note("the workload is released")

        self.origin = self.wait_for(
            lambda: self.workload.of("started"),
            CONNECT_TIMEOUT,
            "the workload is running",
            when=lambda events: events[0]["at"],
        )[0]["at"]

    def stop(self):
        for process in (self.pazarid, self.qemu):
            if process is not None:
                self.note(f"{process.name} stopped", code=process.stop())

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


def execute(manifest, name, options):
    entry = manifest["scenarios"][name]
    scenario = Scenario.of(name, entry)

    scenario.preflight()
    check = scenario.check()

    run = Run(scenario, options.runs)
    (run.directory / "scenario.json").write_text(
        json.dumps(entry, indent=2) + "\n", encoding="utf-8"
    )

    result, detail = "passed", None

    try:
        run.start(manifest["pazarid"])
        check(run)
        run.note("every assertion held")
    except HarnessFailure as error:
        result, detail = "failed", str(error)
        run.note(f"the scenario failed: {error}")
    except KeyboardInterrupt:
        result, detail = "interrupted", "the operator stopped the run"
        run.note("interrupted")
    finally:
        run.stop()

    elapsed = run.clock()

    if options.plot:
        from .plot import render

        try:
            render(run, run.directory / "plot.svg")
            run.note("the plot is plot.svg")
        except Exception as error:  # noqa: BLE001
            run.note(f"the plot could not be drawn: {error}")

    run.record(result, detail)

    return result, detail, elapsed, run.directory
