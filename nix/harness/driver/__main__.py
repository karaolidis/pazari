"""`nix run .#harness [-- <scenario>]`."""

import argparse
import json
import os
import sys
from pathlib import Path

DEFAULT_RUNS = "harness-runs"


def catalogue(manifest):
    lines = ["the scenarios this harness carries:", ""]

    for name, scenario in sorted(manifest["scenarios"].items()):
        lines.append(f"  {name:<24} {scenario['description']}")

    return "\n".join(lines + ["", "run one with: nix run .#harness -- <scenario>"])


def main(argv=None):
    parser = argparse.ArgumentParser(
        prog="pazari-harness",
        description="Runs one pazari scenario in one microvm on this machine.",
    )

    parser.add_argument("scenario", nargs="?", help="the scenario to run")
    parser.add_argument("--manifest", required=True, help=argparse.SUPPRESS)
    parser.add_argument(
        "--runs",
        default=DEFAULT_RUNS,
        help=f"where run directories are made (default: ./{DEFAULT_RUNS})",
    )
    parser.add_argument(
        "--no-plot",
        dest="plot",
        action="store_false",
        help="skip rendering the run's SVG",
    )

    options = parser.parse_args(sys.argv[1:] if argv is None else argv)
    manifest = json.loads(Path(options.manifest).read_text(encoding="utf-8"))

    if options.scenario is None:
        print(catalogue(manifest))

        return 0

    if options.scenario not in manifest["scenarios"]:
        print(f"no scenario called {options.scenario!r}\n", file=sys.stderr)
        print(catalogue(manifest), file=sys.stderr)

        return 2

    options.runs = Path(options.runs).resolve()
    options.runs.mkdir(parents=True, exist_ok=True)
    os.environ.setdefault("MPLCONFIGDIR", str(options.runs / ".matplotlib"))

    from .run import execute
    from .waiting import HarnessFailure

    scenario = manifest["scenarios"][options.scenario]
    print(f"running {options.scenario}: {scenario['description']}")

    try:
        result, detail, elapsed, directory = execute(
            manifest, options.scenario, options
        )
    except HarnessFailure as error:
        print(f"\n{error}", file=sys.stderr)

        return 1

    print(f"\n{options.scenario}: {result} in {elapsed:.1f}s")

    if detail is not None:
        print(detail, file=sys.stderr)

    print(directory)

    return 0 if result == "passed" else 1


if __name__ == "__main__":
    sys.exit(main())
