{
  lib,
  pkgs,
  nixosSystem,
  nixosModules,
}:
let
  evaluate =
    module: configuration:
    (nixosSystem {
      modules = [
        module
        configuration
        {
          nixpkgs.pkgs = pkgs;
          boot.isContainer = true;
          system.stateVersion = lib.trivial.release;
        }
      ];
    }).config;

  render =
    machine: name: builtins.fromTOML (builtins.readFile machine.environment.etc.${name}.source);

  verify =
    name: conditions:
    let
      failed = lib.filter (condition: !condition.ok) conditions;
    in
    if failed == [ ] then
      pkgs.runCommand name { } "touch $out"
    else
      throw "${name}: ${lib.concatMapStringsSep "; " (condition: condition.name) failed}";

  guest = evaluate nixosModules.guest {
    services.pazari-agent = {
      enable = true;
      metricsPort = 9651;
      openFirewall = true;

      settings = {
        engine = {
          service_reserve = "256MiB";
          burst_reserve = "256MiB";
        };

        transport = {
          kind = "vsock";
          cid = 2;
        };
      };
    };
  };

  guestSettings = render guest "pazari/agent.toml";

  guestUnit = guest.systemd.services.pazari-agent.serviceConfig;

  borrowed = evaluate nixosModules.guest {
    services.pazari-agent = {
      enable = true;
      slice = "tenants.slice";
      watchdog.enable = false;

      settings.runtime = {
        backend.kind = "cgroupfs";
      };
    };
  };

  borrowedSettings = render borrowed "pazari/agent.toml";

  borrowedUnit = borrowed.systemd.services.pazari-agent.serviceConfig;

  patched =
    kernelPackages:
    evaluate nixosModules.guest {
      boot = { inherit kernelPackages; };

      services.pazari-agent = {
        enable = true;
        kernel.patches = true;
      };
    };

  patchedLatest = patched pkgs.linuxPackages_latest;

  failed =
    machine:
    map (assertion: assertion.message) (
      lib.filter (assertion: !assertion.assertion) machine.assertions
    );

  host = evaluate nixosModules.host {
    services.pazarid = {
      enable = true;

      settings = {
        policy.host_floor = "2GiB";

        guests.example = {
          ceiling = "8GiB";

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
  };

  hostSettings = render host "pazari/pazarid.toml";

  hostUnit = host.systemd.services.pazarid.serviceConfig;
in
{
  module-guest = verify "module-guest" [
    {
      name = "the workload cgroup is the slice's";
      ok = guestSettings.runtime.cgroup == "/sys/fs/cgroup/workload.slice";
    }
    {
      name = "the limits go through the slice unit";
      ok =
        guestSettings.runtime.backend == {
          kind = "systemd";
          unit = "workload.slice";
        };
    }
    {
      name = "the settings the deployment sets survive";
      ok = guestSettings.engine.burst_reserve == "256MiB" && guestSettings.transport.kind == "vsock";
    }
    {
      name = "the metrics port becomes a listen address";
      ok = guestSettings.metrics.listen == "0.0.0.0:9651";
    }
    {
      name = "the metrics port is opened in the firewall";
      ok = lib.elem 9651 guest.networking.firewall.allowedTCPPorts;
    }
    {
      name = "the slice is declared";
      ok = guest.systemd.units ? "workload.slice";
    }
    {
      name = "systemd-oomd is kept off the slice";
      ok =
        guest.systemd.slices.workload.sliceConfig.ManagedOOMMemoryPressure == "auto"
        && guest.systemd.slices.workload.sliceConfig.ManagedOOMSwap == "auto"
        && guest.systemd.slices.workload.sliceConfig.ManagedOOMPreference == "omit"
        && guest.systemd.slices.workload.sliceConfig.MemoryAccounting;
    }
    {
      name = "the kernel is booted the way preflight wants it";
      ok = lib.all (parameter: lib.elem parameter guest.boot.kernelParams) [
        "transparent_hugepage=madvise"
        "psi=1"
        "memhp_default_state=online_movable"
      ];
    }
    {
      name = "the patches are found for the kernel in use";
      ok = failed patchedLatest == [ ] && patchedLatest.boot.kernelPatches != [ ];
    }
    {
      name = "a kernel without patches is refused";
      ok = failed (patched pkgs.linuxPackages_6_18) != [ ];
    }
    {
      name = "every death path reopens the brake";
      ok = lib.hasInfix "panic-open --config /nix/store/" guestUnit.ExecStopPost;
    }
    {
      name = "the agent is started against the rendered configuration";
      ok = lib.hasInfix "/bin/pazari-agent --config /nix/store/" guestUnit.ExecStart;
    }
    {
      name = "the agent runs outside the slice it brakes";
      ok = guestUnit.Slice == "system.slice";
    }
    {
      name = "the unit is a watchdogged notify service that always comes back";
      ok = guestUnit.Type == "notify" && guestUnit.WatchdogSec == "30s" && guestUnit.Restart == "always";
    }
    {
      name = "the agent may speak vsock, netlink and unix";
      ok = lib.all (family: lib.elem family guestUnit.RestrictAddressFamilies) [
        "AF_VSOCK"
        "AF_NETLINK"
        "AF_UNIX"
      ];
    }
    {
      name = "cgroupfs and the kernel tunables stay writable";
      ok = !(guestUnit ? ProtectControlGroups) && !(guestUnit ? ProtectKernelTunables);
    }
    {
      name = "a slice of the deployment's own is taken whole";
      ok =
        borrowedSettings.runtime.cgroup == "/sys/fs/cgroup/tenants.slice"
        && borrowedSettings.runtime.backend == { kind = "cgroupfs"; }
        && borrowed.systemd.units ? "tenants.slice";
    }
    {
      name = "a watchdog turned off leaves no WatchdogSec behind";
      ok = !(borrowedUnit ? WatchdogSec);
    }
  ];

  module-host = verify "module-host" [
    {
      name = "the policy the deployment sets survives";
      ok = hostSettings.policy.host_floor == "2GiB";
    }
    {
      name = "a guest carries its ceiling, transport and device";
      ok =
        hostSettings.guests.example.ceiling == "8GiB"
        && hostSettings.guests.example.transport.cid == 3
        && hostSettings.guests.example.qmp.path == "/var/lib/microvms/example/qmp-pazari.sock"
        && hostSettings.guests.example.qmp.device == "vm0";
    }
    {
      name = "the daemon is started against the rendered configuration";
      ok = lib.hasInfix "/bin/pazarid --config /nix/store/" hostUnit.ExecStart;
    }
    {
      name = "the daemon runs as its own user, in the hypervisor's group";
      ok =
        hostUnit.User == "pazarid"
        && hostUnit.Group == "kvm"
        && host.users.users.pazarid.isSystemUser
        && host.users.users.pazarid.group == "kvm";
    }
    {
      name = "the unit is a watchdogged notify service that always comes back";
      ok = hostUnit.Type == "notify" && hostUnit.WatchdogSec == "30s" && hostUnit.Restart == "always";
    }
    {
      name = "the daemon may speak vsock and unix, and needs no capability";
      ok =
        lib.all (family: lib.elem family hostUnit.RestrictAddressFamilies) [
          "AF_VSOCK"
          "AF_UNIX"
        ]
        && hostUnit.CapabilityBoundingSet == "";
    }
  ];
}
