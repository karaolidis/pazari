"""Incremental readers over the append-only files a run produces."""

import json


class Tail:
    """A text file read forward, one whole line at a time."""

    def __init__(self, path):
        self.path = path
        self.lines = []
        self._offset = 0
        self._partial = ""

    def poll(self):
        if self.path.exists():
            with self.path.open("rb") as handle:
                handle.seek(self._offset)
                raw = handle.read()

            self._offset += len(raw)
            self._partial += raw.decode("utf-8", errors="replace")

            while "\n" in self._partial:
                line, self._partial = self._partial.split("\n", 1)
                self.take(line)

        return self.lines

    def take(self, line):
        self.lines.append(line)

    def carries(self, needle):
        return any(needle in line for line in self.poll())


class Jsonl(Tail):
    """The workload's event stream, stamped with when the harness first saw it."""

    def __init__(self, path, clock):
        super().__init__(path)

        self.clock = clock
        self.events = []

    def take(self, line):
        line = line.strip()

        if not line:
            return

        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            return

        event["at"] = self.clock()
        self.events.append(event)

    def of(self, kind, **fields):
        self.poll()

        return [
            event
            for event in self.events
            if event.get("type") == kind
            and all(event.get(name) == value for name, value in fields.items())
        ]

    def progress(self, index=None):
        return (
            self.of("progress") if index is None else self.of("progress", index=index)
        )
