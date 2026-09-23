"""Scrapes both Prometheus endpoints on a fixed cadence into one samples file."""

import json
import threading
import urllib.error
import urllib.request

from prometheus_client.openmetrics.parser import text_string_to_metric_families

TIMEOUT = 2.0


class Sampler:
    """One thread scraping every endpoint, keeping the history it writes out."""

    def __init__(self, endpoints, path, interval, clock):
        self.endpoints = dict(endpoints)
        self.interval = interval
        self.clock = clock

        self._file = path.open("w", encoding="utf-8")
        self._lock = threading.Lock()
        self._history = {name: [] for name in self.endpoints}
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._loop, name="sampler", daemon=True)

    def start(self):
        self._thread.start()

    def stop(self):
        self._stop.set()
        self._thread.join(timeout=5 * self.interval + TIMEOUT)
        self._file.close()

    def view(self, name):
        return View(self, name)

    def history(self, name):
        with self._lock:
            return list(self._history[name])

    def _loop(self):
        while not self._stop.is_set():
            for name, url in self.endpoints.items():
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
        self.name = name

    def records(self):
        return self._sampler.history(self.name)

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
        out = []

        for record in self.records():
            if not record["ok"]:
                continue

            found = values(record, name, labels)

            if found:
                out.append((record["at"], found[0]))

        return out

    def totals(self, name):
        return [
            (record["at"], sum(values(record, name)))
            for record in self.records()
            if record["ok"] and record["families"].get(name)
        ]

    def winner(self, name, key):
        out = []

        for record in self.records():
            if not record["ok"]:
                continue

            live = [
                sample["labels"].get(key)
                for sample in record["families"].get(name, [])
                if sample["value"] == 1
            ]

            if live:
                out.append((record["at"], live[0]))

        return out

    def peak(self, name, labels=None):
        return max((value for _, value in self.series(name, labels)), default=None)


def values(record, name, labels=None):
    wanted = labels or {}

    return [
        sample["value"]
        for sample in record["families"].get(name, [])
        if all(sample["labels"].get(key) == value for key, value in wanted.items())
    ]
