{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.pazari-agent;

  format = pkgs.formats.toml { };

  configFile = format.generate "agent.toml" cfg.settings;

  agent = lib.getExe' cfg.package "pazari-agent";

  kernelVersion = lib.versions.majorMinor config.boot.kernelPackages.kernel.version;

  patches = ../patches + "/linux-${kernelVersion}";
in
{
  options.services.pazari-agent = {
    enable = lib.mkEnableOption "the pazari guest memory agent";

    package = lib.mkPackageOption pkgs "pazari" { };

    slice = lib.mkOption {
      type = lib.types.str;
      default = "workload.slice";
      description = ''
        The systemd slice the agent brakes with `memory.high` and `memory.max`.
      '';
    };

    watchdog = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = ''
          Whether systemd watchdogs the agent.
        '';
      };

      interval = lib.mkOption {
        type = lib.types.str;
        default = "30s";
        example = "1min";
        description = ''
          `WatchdogSec` for the agent's unit.
        '';
      };
    };

    logLevel = lib.mkOption {
      type = lib.types.str;
      default = "info";
      example = "pazari_agent=debug";
      description = ''
        `RUST_LOG` for the agent.
      '';
    };

    metricsPort = lib.mkOption {
      type = lib.types.nullOr lib.types.port;
      default = null;
      description = ''
        The port the agent answers Prometheus scrapes on.
      '';
    };

    openFirewall = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        Whether to open `metricsPort` in the guest's firewall.
      '';
    };

    kernel = {
      movable = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = ''
          Whether hot-plugged memory is onlined to `ZONE_MOVABLE`.
        '';
      };

      patches = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = ''
          Whether to patch the guest kernel so that hot-plugged memory hosts its own memmap, and inodes cached in `ZONE_MOVABLE` can be reclaimed.
        '';
      };
    };

    settings = lib.mkOption {
      type = format.type;
      default = { };
      example = {
        workload = {
          service_reserve = "256MiB";
          burst_reserve = "256MiB";
        };

        host.transport.kind = "vsock";
      };
      description = ''
        The agent's configuration, rendered to `/etc/pazari/agent.toml`.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.openFirewall -> cfg.metricsPort != null;
        message = "services.pazari-agent.openFirewall has no port to open without services.pazari-agent.metricsPort.";
      }
      {
        assertion = cfg.kernel.patches -> builtins.pathExists patches;
        message = "services.pazari-agent.kernel.patches has no patches for Linux ${kernelVersion}.";
      }
    ];

    boot = {
      kernelParams = [
        "transparent_hugepage=madvise"
        "psi=1"
      ]
      ++ lib.optional cfg.kernel.movable "memhp_default_state=online_movable"
      ++ lib.optionals cfg.kernel.patches [
        "memory_hotplug.memmap_on_memory=1"
        "virtio_mem.memmap_on_memory=1"
      ];

      kernelPatches = lib.mkIf (cfg.kernel.patches && builtins.pathExists patches) (
        map (patch: {
          name = lib.removeSuffix ".patch" (baseNameOf patch);
          inherit patch;
        }) (lib.filesystem.listFilesRecursive patches)
      );

      kernel.sysctl = lib.mkIf cfg.kernel.movable {
        "vm.dirty_bytes" = lib.mkDefault (256 * 1024 * 1024);
        "vm.dirty_background_bytes" = lib.mkDefault (64 * 1024 * 1024);
      };
    };

    services.pazari-agent.settings = {
      workload = {
        cgroup = lib.mkDefault "/sys/fs/cgroup/${cfg.slice}";

        backend = lib.mkDefault {
          kind = "systemd";
          unit = cfg.slice;
        };
      };
    }
    // lib.optionalAttrs (cfg.metricsPort != null) {
      metrics.listen = lib.mkDefault "0.0.0.0:${toString cfg.metricsPort}";
    };

    environment.etc."pazari/agent.toml".source = configFile;

    networking.firewall.allowedTCPPorts = lib.mkIf (cfg.openFirewall && cfg.metricsPort != null) [
      cfg.metricsPort
    ];

    systemd = {
      slices = {
        ${lib.removeSuffix ".slice" cfg.slice} = {
          description = lib.mkDefault "pazari-managed workload slice";

          sliceConfig = {
            MemoryAccounting = lib.mkDefault true;
            ManagedOOMMemoryPressure = lib.mkDefault "auto";
            ManagedOOMSwap = lib.mkDefault "auto";
            ManagedOOMPreference = lib.mkDefault "omit";
          };
        };
      };

      services.pazari-agent = {
        description = "pazari guest memory agent";
        wantedBy = [ "multi-user.target" ];
        wants = [ cfg.slice ];
        after = [ cfg.slice ];

        serviceConfig = {
          Type = "notify";
          NotifyAccess = "main";
          ExecStart = "${agent} --config ${configFile}";
          ExecStopPost = "${agent} panic-open --config ${configFile}";
          WatchdogSec = lib.mkIf cfg.watchdog.enable cfg.watchdog.interval;
          Restart = "always";
          RestartSec = "5s";
          Slice = "system.slice";
          SyslogIdentifier = "pazari-agent";
          Environment = [ "RUST_LOG=${cfg.logLevel}" ];

          NoNewPrivileges = true;
          ProtectSystem = "strict";
          ProtectHome = true;
          PrivateTmp = true;
          ProtectKernelModules = true;
          ProtectKernelLogs = true;
          ProtectClock = true;
          ProtectHostname = true;
          LockPersonality = true;
          MemoryDenyWriteExecute = true;
          RestrictNamespaces = true;
          RestrictRealtime = true;
          RestrictSUIDSGID = true;
          RestrictAddressFamilies = [
            "AF_UNIX"
            "AF_NETLINK"
            "AF_VSOCK"
            "AF_INET"
            "AF_INET6"
          ];
          SystemCallArchitectures = "native";
          SystemCallFilter = [ "@system-service" ];
        };
      };
    };
  };
}
