# pazari operations

This document is the normative guidance for running `pazarid` on a host. The
reference implementation is the `pazarid` crate.

## Capacity

### What the daemon does and does not decide

A guest is never compelled to hand memory back. The host may send
`reclaim_request`, and a guest that does not want to answer it keeps what it
holds until its ceiling; there is no mechanism in the daemon that takes memory
off an unco-operative guest.

### `grant_swap`

```toml
[policy]
grant_swap = false
```

A grow is granted only if what the host has, less everything already promised to
other guests and less what this grow would take, still leaves `host_floor`.
`grant_swap` decides what "what the host has" counts:

| Value             | Grantable                 | Meaning                |
| ----------------- | ------------------------- | ---------------------- |
| `false` (default) | `MemAvailable`            | A grant promises RAM.  |
| `true`            | `MemAvailable + SwapFree` | Free swap counts, too. |

Either way, until `MemAvailable` has been read - and, under `grant_swap = true`,
`SwapFree` as well - every grow is denied with `host_pressure`.

### Resident provisioning

At startup the daemon compares the guests' ceilings and `host_floor` against
`MemTotal + SwapTotal`:

```
sum(ceilings) + host_floor > MemTotal + SwapTotal
```

If that holds, it warns once.

Operators who want the strong guarantee can have it through how they set
ceilings, rather than by asking the daemon to enforce it:

```
sum(ceilings) + host_floor <= MemTotal
```

Every byte any guest could ever hold is then RAM.

### zram

`SwapTotal` and `SwapFree` for a zram device count what the device would hold
uncompressed. What it actually costs in RAM depends on the compression ratio of
whatever ends up in it, which is not known in advance and is worst for the
incompressible pages a memory-hungry workload tends to produce.

Under `grant_swap = true` the daemon is counting that swap into its grants, so
it warns at startup when it finds a zram device in `/proc/swaps`. Treat zram
capacity as an upper bound you will not reach.

## Revocation

```toml
[policy.revocation]
psi = 20.0
step = "256MiB"
```

Leave the table out and the daemon never asks a guest for memory back on its own
initiative.

With the table present, it watches its own `some avg10` in
`/proc/pressure/memory`, and on every tick at or over `psi` it sends one
`reclaim_request` to the guest the arbiter picks. The built-in policy takes the
guest holding the most over its boot memory, asking for `step` bytes, or for
what that guest holds over boot where that is less.

- One guest is asked per tick.
- A guest with a `reclaim_request` already outstanding is left alone until it
  answers or `policy.reclaim_deadline` runs out.
- A guest that answers `reclaim_nak` is left alone until that deadline would
  have passed. In the meantime the host moves on to the next guest holding the
  most over boot.

Revocation is a request, and a guest may refuse it.

**A host asked to revoke must be able to measure pressure.** Where the table is
present and the kernel carries no `/proc/pressure/memory`, the daemon refuses to
start.

Revocation pairs naturally with `grant_swap = true`, where the daemon is
deliberately lending out memory the host may have to page for.

## Deployment

### Per-guest host-side limits

Nothing in the daemon bounds any one guest's share of the host. Under
`grant_swap = true` in particular, one greedy guest can take the free swap into
its own pages and leave the others paging for it.

Put a host-side limit on each VM's own scope or service, so that a guest's
appetite is charged to that guest.

### Choosing a floor

`host_floor` is the memory the host keeps for itself: the hypervisor's own
working set, the daemon, the storage stack, and whatever else shares the host.
It is what the arbiter refuses to grant past, and it is counted into the startup
warning.
