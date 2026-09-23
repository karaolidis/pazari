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

    systemMemoryMin = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "256M";
      description = ''
        `MemoryMin` for `system.slice`, i.e. what the kernel reclaims from the
        guest's own services only as a last resort.
      '';
    };

    settings = lib.mkOption {
      type = format.type;
      default = { };
      example = {
        engine = {
          service_reserve = "256MiB";
          burst_reserve = "256MiB";
          open_guard = "64MiB";
        };

        runtime.boot_mem = "2GiB";

        transport.kind = "vsock";
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
    ];

    boot.kernelParams = [
      "transparent_hugepage=madvise"
      "psi=1"
    ];

    services.pazari-agent.settings = {
      runtime = {
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
      }
      // lib.optionalAttrs (cfg.systemMemoryMin != null) {
        system = {
          overrideStrategy = "asDropin";
          sliceConfig.MemoryMin = cfg.systemMemoryMin;
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
