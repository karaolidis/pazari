"""Spawning and stopping the children a run is made of."""

import os
import signal
import subprocess


class Process:
    """A child whose output goes to the run directory and which always gets reaped."""

    def __init__(self, name, argv, directory, log):
        self.name = name
        self.argv = list(argv)
        self.directory = directory
        self.log = log
        self.child = None
        self.started = False

    def start(self):
        with self.log.open("a" if self.started else "w", encoding="utf-8") as handle:
            self.child = subprocess.Popen(
                self.argv,
                cwd=self.directory,
                stdin=subprocess.DEVNULL,
                stdout=handle,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )

        self.started = True

        return self.child.pid

    def alive(self):
        return self.child is not None and self.child.poll() is None

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
