{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.pazarid;

  format = pkgs.formats.toml { };

  configFile = format.generate "pazarid.toml" cfg.settings;
in
{
  options.services.pazarid = {
    enable = lib.mkEnableOption "the pazari host memory daemon";

    package = lib.mkPackageOption pkgs "pazari" { };

    group = lib.mkOption {
      type = lib.types.str;
      default = "kvm";
      description = ''
        The group the daemon runs as.
      '';
    };

    watchdog = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = ''
          Whether systemd watchdogs the daemon.
        '';
      };

      interval = lib.mkOption {
        type = lib.types.str;
        default = "30s";
        example = "1min";
        description = ''
          `WatchdogSec` for the daemon's unit.
        '';
      };
    };

    settings = lib.mkOption {
      type = format.type;
      default = { };
      example = {
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
      description = ''
        The daemon's configuration, rendered to `/etc/pazari/pazarid.toml`.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.etc."pazari/pazarid.toml".source = configFile;

    users.users.pazarid = {
      description = "pazari host memory daemon";
      isSystemUser = true;
      inherit (cfg) group;
    };

    systemd.services.pazarid = {
      description = "pazari host memory daemon";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];

      serviceConfig = {
        Type = "notify";
        NotifyAccess = "main";
        ExecStart = "${lib.getExe' cfg.package "pazarid"} --config ${configFile}";
        WatchdogSec = lib.mkIf cfg.watchdog.enable cfg.watchdog.interval;
        Restart = "always";
        RestartSec = "5s";
        User = "pazarid";
        Group = cfg.group;
        SyslogIdentifier = "pazarid";

        CapabilityBoundingSet = "";
        NoNewPrivileges = true;
        ProtectSystem = "full";
        ProtectHome = true;
        PrivateTmp = true;
        ProtectControlGroups = true;
        ProtectKernelTunables = true;
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
          "AF_VSOCK"
          "AF_INET"
          "AF_INET6"
        ];
        SystemCallArchitectures = "native";
        SystemCallFilter = [ "@system-service" ];
      };
    };
  };
}
