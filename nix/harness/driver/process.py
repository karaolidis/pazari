"""Spawning and stopping the two processes a run is made of."""

import os
import signal
import subprocess

from .waiting import HarnessFailure


class Process:
    """A child whose output goes to the run directory and which always gets reaped."""

    def __init__(self, name, argv, directory, log, environment=None):
        self.name = name
        self.argv = list(argv)
        self.directory = directory
        self.log = log
        self.environment = dict(os.environ) | (environment or {})
        self.child = None

    def start(self):
        with self.log.open("w", encoding="utf-8") as handle:
            self.child = subprocess.Popen(
                self.argv,
                cwd=self.directory,
                stdin=subprocess.DEVNULL,
                stdout=handle,
                stderr=subprocess.STDOUT,
                env=self.environment,
                start_new_session=True,
            )

        return self.child.pid

    def alive(self):
        return self.child is not None and self.child.poll() is None

    def require_alive(self):
        if self.child is not None and not self.alive():
            raise HarnessFailure(
                f"{self.name} exited with {self.child.returncode}; see {self.log}"
            )

    def stop(self, grace=10.0):
        if self.child is None:
            return None

        if self.alive():
            self._signal(signal.SIGTERM)

            try:
                self.child.wait(timeout=grace)
            except subprocess.TimeoutExpired:
                self._signal(signal.SIGKILL)
                self.child.wait(timeout=grace)

        return self.child.returncode

    def _signal(self, number):
        try:
            os.killpg(os.getpgid(self.child.pid), number)
        except ProcessLookupError:
            pass
