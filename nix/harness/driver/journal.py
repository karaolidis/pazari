"""Following one unit's journal as readable lines"""

import json
import subprocess
import sys
from argparse import ArgumentParser
from datetime import datetime, timezone

PREFIX = "F_"

LEVELS = {
    0: "EMERG",
    1: "ALERT",
    2: "CRIT",
    3: "ERROR",
    4: "WARN",
    5: "NOTICE",
    6: "INFO",
    7: "DEBUG",
}


def text(value):
    if not isinstance(value, list):
        return str(value)

    if value and all(isinstance(item, int) for item in value):
        return bytes(value).decode("utf-8", errors="replace")

    return " ".join(text(item) for item in value)


def stamp(record):
    raw = record.get("__REALTIME_TIMESTAMP")

    try:
        when = datetime.fromtimestamp(int(raw) / 1_000_000, tz=timezone.utc)
    except (TypeError, ValueError):
        return "".ljust(12)

    return when.strftime("%H:%M:%S.%f")[:-3].ljust(12)


def level(record):
    try:
        return LEVELS.get(int(record.get("PRIORITY", 6)), "INFO")
    except (TypeError, ValueError):
        return "INFO"


def render(record):
    who = record.get("SYSLOG_IDENTIFIER") or record.get("_COMM") or "?"

    line = f"{stamp(record)} {level(record):<6} {text(who)}: {text(record.get('MESSAGE', ''))}"

    fields = " ".join(
        f"{key.removeprefix(PREFIX).lower()}={text(value)}"
        for key, value in sorted(record.items())
        if key.startswith(PREFIX)
    )

    return f"{line} {fields}" if fields else line


def follow(unit, lines):
    return subprocess.Popen(
        [
            "journalctl",
            "--follow",
            "--no-pager",
            "--output=json",
            "--lines",
            str(lines),
            "--unit",
            unit,
        ],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        text=True,
        errors="replace",
    )


def main(argv=None):
    parser = ArgumentParser(description="Follow a unit's journal, fields and all.")
    parser.add_argument("--unit", required=True)
    parser.add_argument("--lines", default="500")

    arguments = parser.parse_args(argv)
    journal = follow(arguments.unit, arguments.lines)

    try:
        for line in journal.stdout:
            line = line.strip()

            if not line:
                continue

            try:
                record = json.loads(line)
            except json.JSONDecodeError:
                print(line, flush=True)
                continue

            print(render(record), flush=True)
    except KeyboardInterrupt:
        pass
    finally:
        journal.terminate()
        journal.wait()

    return journal.returncode


if __name__ == "__main__":
    sys.exit(main())
