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

## Metrics

```toml
[metrics]
listen = "127.0.0.1:9650"
```

With the table present, the process answers `GET` on `listen` with the
OpenMetrics text exposition, `application/openmetrics-text; version=1.0.0`, and
answers any other method with `405`.

The daemon refreshes its gauges once per `tick_interval`; the agent refreshes
its own on every event it handles.

### What the daemon publishes

| Series                                    | Type    | Labels   | Meaning                                                              |
| ----------------------------------------- | ------- | -------- | -------------------------------------------------------------------- |
| `pazarid_guest_size_bytes`                | gauge   | `guest`  | What the guest's memory device holds.                                |
| `pazarid_guest_ceiling_bytes`             | gauge   | `guest`  | The most memory the guest may hold.                                  |
| `pazarid_guest_connected`                 | gauge   | `guest`  | 1 while the guest is connected and past the handshake.               |
| `pazarid_guest_stale`                     | gauge   | `guest`  | 1 while the guest has gone quiet for `policy.stale_after` intervals. |
| `pazarid_guest_reclaim_outstanding`       | gauge   | `guest`  | 1 while a `reclaim_request` is waiting on the guest.                 |
| `pazarid_host_available_bytes`            | gauge   |          | `MemAvailable` as the daemon last read it.                           |
| `pazarid_host_swap_free_bytes`            | gauge   |          | `SwapFree` as the daemon last read it.                               |
| `pazarid_host_psi`                        | gauge   |          | The host's memory `some avg10`, in percent of wall time.             |
| `pazarid_connections_total`               | counter |          | Connections taken on.                                                |
| `pazarid_disconnections_total`            | counter |          | Connections lost.                                                    |
| `pazarid_guest_messages_total`            | counter | `kind`   | Messages received from guests, by protocol message type.             |
| `pazarid_grants_total`                    | counter |          | Grants sent.                                                         |
| `pazarid_denies_total`                    | counter | `reason` | Requests refused, by the reason the guest was given.                 |
| `pazarid_reclaim_requests_total`          | counter |          | Reclaim requests sent.                                               |
| `pazarid_closes_total`                    | counter |          | Connections the broker hung up on.                                   |
| `pazarid_device_reconnects_total`         | counter | `guest`  | Times a guest's memory device was opened again.                      |
| `pazarid_device_seed_failures_total`      | counter | `guest`  | Times a device would not say what size it is.                        |
| `pazarid_device_refused_actuations_total` | counter | `guest`  | Resizes a device refused.                                            |

### What the agent publishes

| Series                                   | Type    | Labels   | Meaning                                                                                                              |
| ---------------------------------------- | ------- | -------- | -------------------------------------------------------------------------------------------------------------------- |
| `pazari_agent_mode`                      | gauge   | `mode`   | 1 on the state the agent is in and 0 on the rest: `titrating`, `steady`, `awaiting_grant`, `releasing`, `exhausted`. |
| `pazari_agent_memory_high_bytes`         | gauge   |          | The workload cgroup's `memory.high`.                                                                                 |
| `pazari_agent_memory_max_bytes`          | gauge   |          | The workload cgroup's `memory.max`.                                                                                  |
| `pazari_agent_plugged_bytes`             | gauge   |          | What the guest's memory device holds.                                                                                |
| `pazari_agent_ceiling_bytes`             | gauge   |          | The ceiling the host named.                                                                                          |
| `pazari_agent_band_open`                 | gauge   |          | 1 while the brake is off and the workload may burst.                                                                 |
| `pazari_agent_connected`                 | gauge   |          | 1 while the agent is connected and past the handshake.                                                               |
| `pazari_agent_workload_usage_bytes`      | gauge   |          | What the workload cgroup held at the last tick.                                                                      |
| `pazari_agent_swap_used_bytes`           | gauge   |          | What the guest had swapped out at the last tick.                                                                     |
| `pazari_agent_high_events`               | gauge   |          | The cgroup's `memory.events` `high`, as the kernel counts it.                                                        |
| `pazari_agent_max_events`                | gauge   |          | The cgroup's `memory.events` `max`, as the kernel counts it.                                                         |
| `pazari_agent_psi_some10`                | gauge   |          | The workload's memory `some avg10`, in percent of wall time.                                                         |
| `pazari_agent_throttle_wakes_total`      | counter |          | Times the workload's throttle counters woke the agent.                                                               |
| `pazari_agent_grant_requests_total`      | counter |          | Requests for more memory sent to the host.                                                                           |
| `pazari_agent_grants_total`              | counter |          | Grants the host answered with.                                                                                       |
| `pazari_agent_denies_total`              | counter | `reason` | Refusals the host answered with, by the reason given.                                                                |
| `pazari_agent_offers_total`              | counter |          | Offers to release memory sent to the host.                                                                           |
| `pazari_agent_reclaims_total`            | counter |          | Reclaims started on the workload cgroup.                                                                             |
| `pazari_agent_reclaim_asked_bytes_total` | counter |          | Memory those reclaims asked the kernel for.                                                                          |
| `pazari_agent_exhaustions_total`         | counter |          | Times the agent fell into `exhausted`.                                                                               |
| `pazari_agent_connections_total`         | counter |          | Connections to the host that reached the handshake.                                                                  |
| `pazari_agent_disconnections_total`      | counter |          | Connections to the host that were lost.                                                                              |

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

### NixOS modules

The flake carries an overlay and two modules, `nixosModules.host` for the
daemon and `nixosModules.guest` for the agent.

```nix
{
  nixpkgs.overlays = [ inputs.pazari.overlays.default ];
  imports = [ inputs.pazari.nixosModules.host ];
}
```

Both take the schema above as a freeform `settings` attribute set and render it.

`services.pazarid` runs the daemon as the `pazarid` user in the group named by
`services.pazarid.group`, which defaults to `kvm` because that is the group the
hypervisor's QMP sockets belong to.

A vsock listener wants the host's module loaded:

```nix
boot.kernelModules = [ "vhost_vsock" ];
```

`services.pazari-agent` declares the slice it brakes, keeps systemd-oomd off it,
boots the guest with the kernel parameters preflight asks for, and runs
`pazari-agent panic-open` from `ExecStopPost`.

### Giving a guest a device to grow into

```nix
microvm.vms.example.config.microvm = {
  mem = 2 * 1024;
  hotplugMem = 30 * 1024;
  hotpluggedMem = 0;

  qemu.machine = "q35";
};
```

`hotplugMem` is the device's whole capacity and `hotpluggedMem` what it starts
plugged with.

QEMU's `microvm` machine model carries no virtio-mem, so the model has to be
`q35`; microvm.nix gives the device the id `vm0`.

### A QMP socket of the daemon's own

microvm.nix drives its own QMP socket, and the daemon holds a connection open.
Give it a second socket rather than have the two of them share one:

```nix
{
  microvm.vms.example.config.microvm.qemu.extraArgs = [
    "-qmp"
    "unix:/var/lib/microvms/example/qmp-pazari.sock,server=on,wait=off"
  ];

  systemd.services."microvm@example".serviceConfig.UMask = "0007";
}
```

### Declaring a guest to the daemon

```nix
services.pazarid = {
  enable = true;

  settings = {
    policy.host_floor = "4GiB";

    guests.example = {
      ceiling = "32GiB";

      transport = {
        kind = "vsock";
        cid = 3;
      };

      qmp = {
        path = "/var/lib/microvms/example/qmp-pazari.sock";
        device = "vm0";
      };
    };
  };
};
```

The guest's half:

```nix
services.pazari-agent = {
  enable = true;

  settings = {
    engine = {
      service_reserve = "512MiB";
      burst_reserve = "256MiB";
      open_guard = "64MiB";
    };

    runtime.boot_mem = "2GiB";

    transport.kind = "vsock";
  };
};
```

### Putting the workload in the slice

Memory the agent cannot see is memory it cannot negotiate for. Every workload
the guest exists to run belongs in the slice, and what puts it there is whatever
creates its cgroup - which is not always the unit you declared.

A quadlet container is its unit. Quadlet runs podman at `CgroupsMode=split`,
which puts the container's payload in a sub-cgroup of the unit's own, so the
unit's slice is the container's slice:

```nix
virtualisation.quadlet.containers.example.serviceConfig.Slice = "workload.slice";
```

A container that some daemon starts on the workload's behalf is not. Where the
workload holds a docker or podman socket and asks for containers through it -
CI runners are the usual case - the daemon on the other end creates them under
its own default parent.

Docker on a cgroup v2 host drives systemd and parents its scopes at
`system.slice`, so those containers land as `system.slice/docker-<id>.scope`:
charged to the guest's own services, watched by whatever manages `system.slice`,
and invisible to the agent. Point the daemon at the slice instead:

```nix
virtualisation.docker.daemon.settings.cgroup-parent = "workload.slice";
```

The daemon itself stays out of the slice. It is a service of the guest rather
than the workload, and what it holds belongs in `service_reserve`.

### systemd-oomd and the brake

The module sets, on the slice:

```
ManagedOOMMemoryPressure=auto
ManagedOOMSwap=auto
ManagedOOMPreference=omit
```

A braked workload sits at `memory.high` and stalls there on purpose, which is
indistinguishable from a workload in trouble to anything watching pressure.
