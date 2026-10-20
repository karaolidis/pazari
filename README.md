# pazari

Guest-initiated memory negotiation for virtual machines.

VMs are usually given a fixed amount of memory, chosen before anyone knows what
the workload needs. With `pazari`, the guest asks for memory when it needs it
and gives it back when it doesn't. An agent in the guest throttles the workload
with the cgroup `memory.high` limit before it runs out, and asks a daemon on the
host for more while it is throttled.

The name is Greek for haggling: παζάρι, /paˈza.ɾi/.

## Components

- `pazari-agent` runs in the guest. It throttles the workload cgroup and asks
  the host for memory.
- `pazarid` runs on the host. It decides which guests may grow and resizes their
  virtio-mem devices over QMP.
- `pazari-stress` generates predictable memory load for testing.

## Quickstart

On the host:

```nix
{
  nixpkgs.overlays = [ inputs.pazari.overlays.default ];

  imports = [ inputs.pazari.nixosModules.host ];

  boot.kernelModules = [ "vhost_vsock" ];

  services.pazarid = {
    enable = true;

    settings = {
      grant.floor = "4GiB";

      guests.example = {
        ceiling = "32GiB";
        transport = { kind = "vsock"; cid = 3; };
        qmp = {
          path = "/var/lib/microvms/example/qmp-pazari.sock";
          device = "vm0";
        };
      };
    };
  };
}
```

In the guest:

```nix
{
  imports = [ inputs.pazari.nixosModules.guest ];

  services.pazari-agent = {
    enable = true;

    settings = {
      workload = {
        service_reserve = "512MiB";
        burst_reserve = "256MiB";
      };

      host.transport.kind = "vsock";
    };
  };
}
```

The VM needs a virtio-mem device to grow into, and a QMP socket for the daemon
separate from the one microvm.nix uses. The `UMask` makes that socket accessible
to the daemon's group.

```nix
{
  microvm.vms.example.config.microvm = {
    mem = 2 * 1024;
    hotplugMem = 30 * 1024;
    hotpluggedMem = 0;

    qemu.machine = "q35";

    qemu.extraArgs = [
      "-qmp"
      "unix:/var/lib/microvms/example/qmp-pazari.sock,server=on,wait=off"
    ];
  };

  systemd.services."microvm@example".serviceConfig.UMask = "0007";
}
```

See `pazari(7)` for guest kernel requirements, sizing boot memory, and running
containerised workloads. The guest module onlines plugged memory to
`ZONE_MOVABLE` and caps the dirty limits. Setting
`services.pazari-agent.kernel.patches` applies the kernel patches it describes,
if `nix/patches` has them for the guest's kernel.

## Documentation

| Page                   | Covers                                       |
| ---------------------- | -------------------------------------------- |
| `pazari(7)`            | Overview and deployment.                     |
| `pazari-agent(1)`      | The guest agent, its commands and preflight. |
| `pazarid(1)`           | The host daemon.                             |
| `pazari-stress(1)`     | The load generator and its output.           |
| `pazari-agent.toml(5)` | Agent settings and sizing the reserves.      |
| `pazarid.toml(5)`      | Daemon settings.                             |

Annotated example configurations are in `examples/`.

## Development

```sh
nix develop
cargo check --workspace
cargo test --workspace
nix build
nix fmt
```

Integration tests and measurements run in VMs:

```sh
nix flake check
nix build .#checks.x86_64-linux.test-grow-burst -L
nix build .#measure-brake-hold -L
```

See `nix/harness/README.md` for writing scenarios and reading their results.
