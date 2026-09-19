# pazari

Guest-initiated VM memory negotiation framework.

Virtual machines are usually sized by guesswork: a static allocation that is
either wasteful or too small. `pazari` ("παζάρι", "paˈza.ɾi", i.e. negotiation,
haggling, bargaining) inverts that. A guest agent watches the workload from
inside the VM, brakes it with the cgroup `memory.high` throttle instead of
letting it OOM, and asks a host daemon for more memory. The host daemon
arbitrates requests across all guests and actuates the grant by resizing the
VM's virtio-mem device.

## Components

- `pazari-agent` - guest agent; cgroup-driven detection, braking, and requests.
- `pazarid` - host daemon; arbitration across guests and virtio-mem actuation.
- `pazari-stress` - deterministic memory workload generator for testing.

## Development

```sh
nix develop
cargo check --workspace
cargo test --workspace
nix build
nix fmt
```
