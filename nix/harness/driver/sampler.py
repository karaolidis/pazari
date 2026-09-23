"""Scrapes both Prometheus endpoints on a fixed cadence into one samples file."""

import json
import threading
import urllib.error
import urllib.request

from prometheus_client.openmetrics.parser import text_string_to_metric_families

TIMEOUT = 2.0


class Sampler:
    """A thread per endpoint."""

    def __init__(self, endpoints, path, interval, clock):
        self.endpoints = dict(endpoints)
        self.interval = interval
        self.clock = clock

        self._file = path.open("w", encoding="utf-8")
        self._lock = threading.Lock()
        self._history = {name: [] for name in self.endpoints}
        self._stop = threading.Event()
        self._threads = [
            threading.Thread(
                target=self._loop,
                args=(name, url),
                name=f"sampler-{name}",
                daemon=True,
            )
            for name, url in self.endpoints.items()
        ]

    def start(self):
        for thread in self._threads:
            thread.start()

    def stop(self):
        self._stop.set()

        for thread in self._threads:
            thread.join(timeout=5 * self.interval + TIMEOUT)

        self._file.close()

    def view(self, name):
        return View(self, name)

    def since(self, name, index):
        with self._lock:
            return self._history[name][index:]

    def _loop(self, name, url):
        while not self._stop.is_set():
            self._scrape(name, url)
            self._stop.wait(self.interval)

    def _scrape(self, name, url):
        record = {"at": self.clock(), "source": name}

        try:
            with urllib.request.urlopen(url, timeout=TIMEOUT) as answer:
                text = answer.read().decode("utf-8")

            families = {}

            for family in text_string_to_metric_families(text):
                for sample in family.samples:
                    families.setdefault(sample.name, []).append(
                        {"labels": dict(sample.labels), "value": sample.value}
                    )

            record |= {"ok": True, "families": families}
        except (urllib.error.URLError, OSError, ValueError) as error:
            record |= {"ok": False, "error": str(error)}

        with self._lock:
            self._history[name].append(record)
            self._file.write(json.dumps(record) + "\n")
            self._file.flush()


class View:
    """What one endpoint has said so far, in the order it said it."""

    def __init__(self, sampler, name):
        self._sampler = sampler
        self._records = []
        self._series = {}
        self.name = name

    def records(self):
        self._records += self._sampler.since(self.name, len(self._records))

        return self._records

    def answered(self):
        records = self.records()

        return bool(records) and records[-1]["ok"]

    def spoke(self, ok=True, since=None):
        for record in self.records():
            if record["ok"] == ok and (since is None or record["at"] >= since):
                return (record["at"],)

        return None

    def latest(self, name, labels=None):
        for record in reversed(self.records()):
            if record["ok"]:
                return next(iter(values(record, name, labels)), None)

        return None

    def first(self, name, at, labels=None):
        for record in self.records():
            if record["ok"] and record["at"] >= at:
                return next(iter(values(record, name, labels)), 0.0)

        return 0.0

    def series(self, name, labels=None):
        return self._points(
            ("series", name, key(labels)),
            lambda record: point(record, name, labels),
        )

    def totals(self, name):
        return self._points(
            ("totals", name),
            lambda record: (
                (record["at"], sum(values(record, name)))
                if record["families"].get(name)
                else None
            ),
        )

    def winner(self, name, field):
        def live(record):
            held = [
                sample["labels"].get(field)
                for sample in record["families"].get(name, [])
                if sample["value"] == 1
            ]

            return (record["at"], held[0]) if held else None

        return self._points(("winner", name, field), live)

    def _points(self, reading, pick):
        points, scanned = self._series.setdefault(reading, ([], 0))
        records = self.records()

        for record in records[scanned:]:
            found = pick(record) if record["ok"] else None

            if found is not None:
                points.append(found)

        self._series[reading] = (points, len(records))

        return points

    def peak(self, name, labels=None):
        return max((value for _, value in self.series(name, labels)), default=None)


def key(labels):
    return tuple(sorted((labels or {}).items()))


def point(record, name, labels=None):
    found = values(record, name, labels)

    return (record["at"], found[0]) if found else None


def values(record, name, labels=None):
    wanted = labels or {}

    return [
        sample["value"]
        for sample in record["families"].get(name, [])
        if all(sample["labels"].get(key) == value for key, value in wanted.items())
    ]
