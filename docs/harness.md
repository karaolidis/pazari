# pazari test harness

This document describes the VM test harness. The reference implementation is
`nix/harness`.

## Running one

```
nix run .#harness                 # list the scenarios
nix run .#harness -- grow-burst   # run one
```

| Option       | Meaning                         |
| ------------ | ------------------------------- |
| `--runs DIR` | Where run directories are made. |
| `--no-plot`  | Skip the SVG.                   |

The host needs `/dev/kvm` and `/dev/vhost-vsock` readable and writable, and the
two metrics ports free.

A scenario exits non-zero if any assertion did not hold.

## A scenario

A scenario is one `.nix` file in `nix/harness/scenarios`.

| Attribute     | Meaning                                                    |
| ------------- | ---------------------------------------------------------- |
| `description` | One line, shown in the listing.                            |
| `timeout`     | Seconds the whole run may take before it is failed.        |
| `daemon`      | What this scenario changes about the host daemon settings. |
| `guest`       | A NixOS module, merged into the guest.                     |
| `assertions`  | Python source declaring one `check(run)`.                  |

`guest` usually only sets `pazari.harness.workload.phases`, the `pazari-stress`
plan the guest is driven with, and `pazari.harness.engine`, what this scenario
changes about the agent's engine.

`assertions` is written against the vocabulary in
`nix/harness/driver/checks.py`.

The workload does not start with the guest. It blocks on a file in the share
that the driver makes only once it is scraping both endpoints and the guest is
past the handshake, so nothing a scenario cares about happens unobserved. The
workload's own `started` event is the run's `t0`.

### Waiting

A check never sleeps. It waits on something the run can observe, and every wait
is anchored so that history cannot satisfy it:

- `counter_rose` waits for a counter to rise past where it stood at `t0`.
- `gauge_reaches` waits for a scrape from the moment the wait began.

Both take `since` to anchor a wait at some other moment a check has in hand.

## The plot

One SVG per run, five panels on a shared time axis that opens at `t0`. The
boot and the handshake are not drawn; the captured streams on disk are whole.

| Panel                | What it carries                                                  |
| -------------------- | ---------------------------------------------------------------- |
| memory               | `plugged`, `memory.max`, `memory.high`, workload usage, ceiling. |
| workload fault rate  | What `pazari-stress` reported, log scale.                        |
| host available       | The daemon's `MemAvailable`.                                     |
| host memory pressure | The daemon's PSI reading.                                        |
| swimlanes            | The engine's mode, and the daemon's view of the guest link.      |

Vertical rules on the memory panel mark the protocol events - a request, a
grant, a deny, an offer, a reclaim.

## A run directory

`harness-runs/<scenario>-<stamp>/`.

| Entry           | What it is                                                      |
| --------------- | --------------------------------------------------------------- |
| `scenario.json` | The manifest entry this run was built from.                     |
| `pazarid.toml`  | The daemon settings, with the QMP socket filled in.             |
| `samples.jsonl` | Every scrape of both endpoints, one object each.                |
| `events.jsonl`  | What the driver noted, and when.                                |
| `result.json`   | The result, and how long it took.                               |
| `plot.svg`      | The plot.                                                       |
| `logs/`         | `qemu.log`, the guest's console and journal, and `pazarid.log`. |
| `share/`        | The 9p share: the workload's event stream, and the gate.        |
