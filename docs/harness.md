# pazari test harness

This document describes the VM test harness. The reference implementation is
`nix/harness`.

```
nix build .#checks.x86_64-linux.test-reconnect -L
nix build .#measure-hotplug-rtt -L
```

## A scenario

A scenario is one `.nix` file in `nix/harness/scenarios`.

| Attribute     | Meaning                                                          |
| ------------- | ---------------------------------------------------------------- |
| `description` | One line, printed when the run starts.                           |
| `timeout`     | Seconds the whole run may take before it is failed.              |
| `breaches`    | Optional. True when the workload is meant to reach `memory.max`. |
| `sample`      | Optional. Seconds between scrapes of every endpoint.             |
| `daemon`      | What this scenario changes about the host daemon's settings.     |
| `guests`      | The guests.                                                      |
| `host`        | Optional. A NixOS module merged into the outer machine.          |
| `assertions`  | Python source declaring one `check(run)`.                        |

Each guest carries `daemon`, what the scenario changes about that guest's entry
in the daemon's settings, and `module`, a NixOS module merged into the guest.
That module usually only sets `services.pazari-agent.harness.workload.phases`,
the `pazari-stress` plan the guest is driven with, and
`services.pazari-agent.harness.engine`, what this scenario changes about the
agent's engine.

`assertions` is written against the vocabulary in
`nix/harness/driver/checks.py`, over the event series in
`nix/harness/driver/series.py`.

The workload does not start with the guest. It blocks on a file in the share
that the driver makes only once it is scraping every endpoint and every guest is
past the handshake, so nothing a scenario cares about happens unobserved. The
first workload's own `started` event is the run's `t0`.

## A measurement

A scenario whose name begins with `measure-` is a measurement rather than a
test. It is kept out of `nix flake check`, and built by name instead:

```
nix build .#measure-brake-crawl -L
```

## Check tooling

| Name                      | What it is                                                |
| ------------------------- | --------------------------------------------------------- |
| `run.daemon`              | The daemon's metrics endpoint.                            |
| `run.guests[name]`        | One guest.                                                |
| `run.guest`               | The only guest, for a scenario that has one.              |
| `run.host`                | The machine the daemon runs on.                           |
| `guest.agent`             | That guest's metrics endpoint.                            |
| `guest.workload`          | That guest's `pazari-stress` event stream.                |
| `guest.labels`            | `{"guest": name}`, for the daemon's per-guest series.     |
| `guest.shell(...)`        | Runs a command inside the guest and waits for its answer. |
| `guest.halt(...)`         | Takes that guest's hypervisor away.                       |
| `run.host.restart_daemon` | Takes the daemon down, and waits for it to answer again.  |
| `run.host.start`          | `systemctl start` on the outer machine.                   |
| `run.host.stop`           | `systemctl stop` on the outer machine.                    |
| `run.host.restart`        | `systemctl restart` on the outer machine.                 |

## The plot

One SVG per guest, five panels on a shared time axis that opens at `t0`. The
boot and the handshake are not drawn; the captured streams on disk are whole.

| Panel                | What it carries                                                    |
| -------------------- | ------------------------------------------------------------------ |
| memory               | device size, `memory.max`, `memory.high`, workload usage, ceiling. |
| workload fault rate  | What `pazari-stress` reported, log scale.                          |
| host available       | The daemon's `MemAvailable`.                                       |
| host memory pressure | The daemon's PSI reading.                                          |
| swimlanes            | The engine's mode, and the daemon's view of the guest link.        |

Vertical rules on the memory panel mark the protocol events - a request, a
grant, a deny, an offer, a reclaim.

## A run directory

`/var/lib/pazari-harness` inside the outer machine, copied out into the check's
result.

| Entry               | What it is                                                        |
| ------------------- | ----------------------------------------------------------------- |
| `scenario.json`     | The manifest this run was built from.                             |
| `samples.jsonl`     | Every scrape of every endpoint, one object each.                  |
| `events.jsonl`      | What the driver noted, and when.                                  |
| `result.json`       | The result, and how long it took.                                 |
| `measurements.json` | What a `measure-` scenario took.                                  |
| `plot.svg`          | The plot, or one `<guest>.svg` each where there is more than one. |
| `logs/`             | `pazarid.log`, and one console per guest.                         |
| `<guest>/`          | Where that guest's qemu runs: its QMP socket, and its share.      |
| `<guest>/share`     | The 9p share: the workload's stream, the gate, the commands.      |
