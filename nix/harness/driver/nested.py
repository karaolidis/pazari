"""The daemon and the guests are units of the machine the driver runs on."""

import argparse
import json
import subprocess
import sys
from pathlib import Path

from .process import Process
from .run import (
    COMMAND_TIMEOUT,
    UNIT_TIMEOUT,
    Run,
    Scenario,
    drive,
    perform,
)
from .waiting import HarnessFailure

BACKLOG = "500"


class Systemd:
    """The machine the daemon runs on, driven through systemctl."""

    def __init__(self, run):
        self._run = run

    def shell(self, command, reason, timeout=COMMAND_TIMEOUT, expect=0):
        issued = self._run.clock()

        try:
            answer = subprocess.run(
                command,
                shell=True,
                capture_output=True,
                text=True,
                timeout=timeout,
                check=False,
            )
        except subprocess.TimeoutExpired as expired:
            raise HarnessFailure(
                f"{command!r} was still running after {timeout:g}s"
            ) from expired

        output = answer.stdout + answer.stderr

        if expect is not None and answer.returncode != expect:
            raise HarnessFailure(
                f"{command!r} answered {answer.returncode}, not {expect}: {output.strip()}"
            )

        self._run.note(reason, at=issued, code=answer.returncode)
        self._run.seen = issued

        return answer.returncode, output

    def start(self, unit, reason=None, timeout=UNIT_TIMEOUT):
        answer = self.shell(
            f"systemctl start {unit}", reason or f"{unit} was started", timeout
        )
        self._run.guard_unit(unit)

        return answer

    def stop(self, unit, reason=None, timeout=UNIT_TIMEOUT):
        self._run.alive.pop(unit, None)

        return self.shell(
            f"systemctl stop {unit}", reason or f"{unit} was stopped", timeout
        )

    def restart(self, unit, reason=None, timeout=UNIT_TIMEOUT):
        answer = self.shell(
            f"systemctl restart {unit}", reason or f"{unit} was restarted", timeout
        )
        self._run.guard_unit(unit)

        return answer

    def restart_daemon(self, reason=None, timeout=UNIT_TIMEOUT):
        answer = self.restart(
            self._run.scenario.daemon["unit"],
            reason or "the daemon was restarted",
            timeout,
        )
        issued = self._run.seen

        self._run.daemon.answering(
            timeout, "the daemon is answering scrapes again", since=self._run.clock()
        )
        self._run.seen = issued

        return answer


class NestedRun(Run):
    """A run whose daemon and whose guests are systemd units beside it."""

    def __init__(self, scenario, directory):
        directory = Path(directory)

        super().__init__(
            scenario,
            directory,
            {name: spec["share"] for name, spec in scenario.guests.items()},
            {name: directory / "logs" / f"{name}.log" for name in scenario.guests},
        )

        self.host = Systemd(self)
        self.journals = [
            self.follow("pazarid", scenario.daemon["unit"], self.logs / "pazarid.log"),
            *(
                self.follow(name, spec["unit"], self.logs / f"{name}.log")
                for name, spec in scenario.guests.items()
            ),
        ]

    def follow(self, name, unit, log):
        return Process(
            f"journal-{name}",
            [
                "journalctl",
                "--follow",
                "--no-pager",
                "--output=short-precise",
                "--lines",
                BACKLOG,
                "--unit",
                unit,
            ],
            self.directory,
            log,
        )

    def bring_up(self):
        for journal in self.journals:
            journal.start()

        for unit in self.units():
            self.guard_unit(unit)

    def guard_unit(self, unit):
        if unit in self.units():
            self.alive[unit] = lambda: self.require_active(unit)

    def halt(self, guest, reason):
        return self.host.stop(guest.spec["unit"], reason)

    def units(self):
        return [self.scenario.daemon["unit"]] + [
            spec["unit"] for spec in self.scenario.guests.values()
        ]

    def require_active(self, unit):
        if subprocess.run(
            ["systemctl", "is-active", "--quiet", unit],
            check=False,
        ).returncode:
            raise HarnessFailure(f"{unit} is no longer running, and the run needs it")

    def stop(self):
        for journal in self.journals:
            journal.stop()

        super().stop()


def execute(manifest, directory):
    entry = manifest["scenario"]
    scenario = Scenario.of(entry)

    return perform(NestedRun(scenario, directory), entry, scenario.check())


def main(argv=None):
    parser = argparse.ArgumentParser(
        prog="pazari-harness-nested",
        description="Runs one pazari scenario against the units of this machine.",
    )

    parser.add_argument("--manifest", required=True, help=argparse.SUPPRESS)
    parser.add_argument("--directory", required=True, help="where the run is recorded")

    options = parser.parse_args(sys.argv[1:] if argv is None else argv)
    manifest = json.loads(Path(options.manifest).read_text(encoding="utf-8"))
    entry = manifest["scenario"]

    return drive(
        entry["name"],
        entry["description"],
        lambda: execute(manifest, options.directory),
    )


if __name__ == "__main__":
    sys.exit(main())
