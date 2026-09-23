# pazari

Guest-initiated VM memory negotiation.

Virtual machines are sized by guesswork: a static allocation chosen before
anyone knows what the workload will do, and so either wasteful or too small.
`pazari` ("παζάρι", "paˈza.ɾi", haggling) inverts that. A guest agent brakes
the workload with the cgroup `memory.high` throttle instead of letting the
kernel kill it, and spends the time the brake buys asking a host daemon for
more memory.

## Components

- `pazari-agent` - guest agent; cgroup-driven detection, braking, and requests.
- `pazarid` - host daemon; arbitration across guests and virtio-mem actuation.
- `pazari-stress` - deterministic memory workload generator for testing.

## Quickstart

```nix
{
  nixpkgs.overlays = [ inputs.pazari.overlays.default ];

  imports = [ inputs.pazari.nixosModules.host ];

  boot.kernelModules = [ "vhost_vsock" ];

  services.pazarid = {
    enable = true;

    settings = {
      policy.host_floor = "4GiB";

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

The guest's half, inside the VM:

```nix
{
  imports = [ inputs.pazari.nixosModules.guest ];

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
}
```

The VM itself needs a virtio-mem device to grow into, and the daemon needs a
QMP socket of its own rather than the one microvm.nix already drives:

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

## Documentation

| Page                   | What it covers                                     |
| ---------------------- | -------------------------------------------------- |
| `pazari(7)`            | What this is, and how to deploy it.                |
| `pazari-agent(1)`      | The guest agent, its subcommands, and preflight.   |
| `pazarid(1)`           | The host daemon.                                   |
| `pazari-stress(1)`     | The workload generator and its event stream.       |
| `pazari-agent.toml(5)` | Every agent setting, and how to size the reserves. |
| `pazarid.toml(5)`      | Every daemon setting.                              |

Annotated configuration files are in `examples/`.

## Development

```sh
nix develop
cargo check --workspace
cargo test --workspace
nix build
nix fmt
```

```sh
nix flake check
nix build .#checks.x86_64-linux.test-grow-burst -L
nix build .#measure-brake-crawl -L
```
