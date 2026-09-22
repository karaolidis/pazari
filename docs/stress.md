# pazari workload generator

This is the normative specification of `pazari-stress`, the deterministic memory
workload the VM test harness runs inside a guest. The reference implementation
is the `pazari-stress` crate.

## The arena and the frontier

The tool holds exactly one mapping, the _arena_: anonymous, private, readable
and writable, sized once at startup.

Inside the arena the tool keeps a _frontier_: the byte offset up to which pages
have been touched.

- A **grow** advances the frontier, writing into pages the run has not touched
  since they were last dropped. Each such write is a page fault.
- A **release** retreats the frontier, dropping the pages behind it with
  `MADV_DONTNEED`.
- A **regrow** advances the frontier back over released pages, faulting them in
  again.
- A **churn** rewrites what is already resident without moving the frontier.
- A **hold** touches nothing.

The run is single-threaded and synchronous.

## Invocation

```
pazari-stress [--interval DURATION] [--arena SIZE] [--no-reserve] [--no-thp]
              [--fill index|random] <PHASE>...
```

| Option         | Default         | Meaning                                                         |
| -------------- | --------------- | --------------------------------------------------------------- |
| `--interval`   | `500ms`         | How often a running phase reports its progress.                 |
| `--arena`      | the plan's peak | How much address space to map.                                  |
| `--no-reserve` | off             | Adds `MAP_NORESERVE`, so the arena is never committed up front. |
| `--no-thp`     | off             | Applies `MADV_NOHUGEPAGE` to the arena.                         |
| `--fill`       | `index`         | What every touched page is filled with.                         |

## Phase grammar

A phase spec is a kind, an `=`, its target, and then comma-separated
`key=value` options:

```
grow=2GiB[,rate=100MiB/s][,chunk=1MiB]
churn=30s,rate=500MiB/s[,chunk=1MiB]
hold=30s
release=1GiB[,rate=...][,chunk=64MiB]
```

| Kind      | Target     | `rate`   | `chunk` default | What it does                                         |
| --------- | ---------- | -------- | --------------- | ---------------------------------------------------- |
| `grow`    | bytes      | optional | `1MiB`          | Advances the frontier by the target.                 |
| `churn`   | a duration | required | `1MiB`          | Rewrites the resident set for the target's duration. |
| `hold`    | a duration | -        | -               | Waits, touching nothing.                             |
| `release` | bytes      | optional | `64MiB`         | Retreats the frontier by the target.                 |

### Page granularity

The frontier is a byte offset, but memory moves in pages.

- A **grow** faults in every page its range reaches, including a page the range
  only ends inside.
- A **release** drops only the pages its range holds whole. Half a page cannot
  go back while the other half is still live, so a range that starts or ends
  inside a page leaves that page resident.

## Fills

| Value    | What a touched page gets                                                           |
| -------- | ---------------------------------------------------------------------------------- |
| `index`  | One word at the start of the page, derived from the page's own ordinal.            |
| `random` | The whole page filled with inline xorshift output, seeded from the page's ordinal. |

`index` is cheap and makes every page distinct. `random` is incompressible, so
zram cannot hold the page for less than it costs.

## Event stream

The tool writes its events to **stdout**. Tracing logs go to **stderr**.

Each object carries a `type` member naming the event in `snake_case`.

Memory quantities are counts of bytes as JSON integers in the `u64` range.
`elapsed_ms` is milliseconds since the process started, taken from a monotonic
clock.

An event stream is exactly one `started`, then for each phase in order one
`phase`, zero or more `progress`, and one `completed`, and then one terminal
event. An interruption or a failure replaces the rest of the stream.

### `started`

The first event.

| Field        | Type        | Meaning                                                      |
| ------------ | ----------- | ------------------------------------------------------------ |
| `type`       | `"started"` | Event tag.                                                   |
| `pid`        | uint        | The workload's process id.                                   |
| `page_size`  | bytes       | The guest's page size.                                       |
| `arena`      | bytes       | What was mapped, rounded up to a whole page.                 |
| `no_reserve` | bool        | Whether the arena was mapped `MAP_NORESERVE`.                |
| `no_thp`     | bool        | Whether huge pages were kept out of the arena.               |
| `fill`       | string enum | `index` or `random`.                                         |
| `plan`       | array       | The parsed plan, one object per phase, in the order it runs. |

Each entry of `plan` carries the `spec` it was parsed from, its `kind`, its
`target_bytes` or `target_ms`, its `chunk`, and its `rate_bps` if it has one.

### `phase`

Emitted once as a phase starts. Exactly one of `target_bytes` and `target_ms` is
present, decided by `kind`.

| Field          | Type        | Meaning                                      |
| -------------- | ----------- | -------------------------------------------- |
| `type`         | `"phase"`   | Event tag.                                   |
| `index`        | uint        | The phase's position in the plan, from zero. |
| `kind`         | string enum | `grow`, `churn`, `hold`, or `release`.       |
| `target_bytes` | bytes       | What a `grow` or a `release` moves.          |
| `target_ms`    | ms          | How long a `churn` or a `hold` runs.         |

### `progress`

Emitted every `--interval` while a phase runs, for every kind of phase.

| Field        | Type         | Meaning                                               |
| ------------ | ------------ | ----------------------------------------------------- |
| `type`       | `"progress"` | Event tag.                                            |
| `index`      | uint         | The running phase.                                    |
| `elapsed_ms` | ms           | Since the process started.                            |
| `touched`    | bytes        | What this phase has moved so far.                     |
| `frontier`   | bytes        | Where the frontier stands now.                        |
| `rate_bps`   | bytes        | What was moved since the last `progress`, per second. |

`touched` counts bytes written for a `grow` and a `churn`, bytes released for a
`release`, and stays zero for a `hold`. It resets at the start of every phase.

A phase that ends before its first interval is due reports no `progress` at all.

### `completed`

Emitted once a phase reaches its target.

| Field        | Type          | Meaning                    |
| ------------ | ------------- | -------------------------- |
| `type`       | `"completed"` | Event tag.                 |
| `index`      | uint          | The phase that finished.   |
| `elapsed_ms` | ms            | Since the process started. |

### `done`

The last event of a run that finished its plan.

| Field        | Type     | Meaning                    |
| ------------ | -------- | -------------------------- |
| `type`       | `"done"` | Event tag.                 |
| `elapsed_ms` | ms       | Since the process started. |

### `interrupted`

The last event of a run stopped by `SIGTERM` or `SIGINT`.

| Field        | Type            | Meaning                     |
| ------------ | --------------- | --------------------------- |
| `type`       | `"interrupted"` | Event tag.                  |
| `index`      | uint            | The phase that was running. |
| `elapsed_ms` | ms              | Since the process started.  |

### `failed`

The last event of a run that could not carry on.

| Field        | Type       | Meaning                    |
| ------------ | ---------- | -------------------------- |
| `type`       | `"failed"` | Event tag.                 |
| `elapsed_ms` | ms         | Since the process started. |
| `error`      | string     | Human-readable, for logs.  |

## Exit codes

| Code | Meaning                                                            |
| ---- | ------------------------------------------------------------------ |
| `0`  | The run ended with `done` or with `interrupted`.                   |
| `1`  | The run ended with `failed`.                                       |
| `2`  | The command line itself was refused, before any event was written. |
