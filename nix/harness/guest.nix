{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.pazari.harness;

  stress = lib.getExe' pkgs.pazari "pazari-stress";

  mac = "02:00:00:0d:11:01";
  address = "10.0.2.15";
  gateway = "10.0.2.2";

  release = pkgs.writeShellScript "pazari-harness-release" ''
    until [ -e "$1" ]; do
      ${lib.getExe' pkgs.coreutils "sleep"} 0.1
    done
  '';

  engine = {
    service_reserve = "640MiB";
    burst_reserve = "256MiB";
    open_guard = "64MiB";

    grow_step = "512MiB";
    deny_backoff = "2s";
    settle_grace = "5s";

    idle_threshold = 0.5;
    idle_intervals = 3;

    shrink_step = "256MiB";
    shrink_cooldown = "5s";

    reclaim_swappiness = 0;

    titrate_step = "4GiB";
    titrate_interval = "500ms";
    titrate_psi_threshold = 5.0;

    exhaustion = {
      kind = "stall";
      timeout = "300s";
    };
  };
in
{
  options.pazari.harness = {
    share = {
      tag = lib.mkOption {
        type = lib.types.str;
        default = "harness";
        description = ''
          The 9p mount tag the observability share is exported under.
        '';
      };

      source = lib.mkOption {
        type = lib.types.str;
        default = "share";
        description = ''
          The directory the share comes from, relative to the run directory
          qemu is started in.
        '';
      };

      mountPoint = lib.mkOption {
        type = lib.types.path;
        default = "/var/lib/pazari-harness";
        description = ''
          Where the share is mounted in the guest.
        '';
      };
    };

    workload = {
      unit = lib.mkOption {
        type = lib.types.str;
        default = "pazari-workload";
        description = ''
          The unit the workload runs as.
        '';
      };

      slice = lib.mkOption {
        type = lib.types.str;
        default = "workload.slice";
        description = ''
          The slice the workload runs in.
        '';
      };

      log = lib.mkOption {
        type = lib.types.str;
        default = "workload.jsonl";
        description = ''
          The file in the share the workload's event stream is written to.
        '';
      };

      gate = lib.mkOption {
        type = lib.types.str;
        default = "go";
        description = ''
          The file in the share whose appearance releases the workload.
        '';
      };

      arguments = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [
          "--interval"
          "250ms"
          "--no-reserve"
        ];
        description = ''
          The options `pazari-stress` runs with, before its phases.
        '';
      };

      phases = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        example = [
          "grow=1GiB,rate=512MiB/s"
          "hold=10s"
        ];
        description = ''
          The phase plan the scenario drives the guest with.
        '';
      };
    };

    metricsPort = lib.mkOption {
      type = lib.types.port;
      default = 9651;
      description = ''
        The port the agent answers scrapes on, inside the guest.
      '';
    };

    forwardedPort = lib.mkOption {
      type = lib.types.port;
      default = 19651;
      description = ''
        The loopback port on the host `metricsPort` is forwarded to.
      '';
    };

    vsockPort = lib.mkOption {
      type = lib.types.port;
      default = 4650;
      description = ''
        The port the agent dials the host daemon on.
      '';
    };

    qmp = {
      socket = lib.mkOption {
        type = lib.types.str;
        default = "qmp.sock";
        description = ''
          The daemon's QMP socket, relative to the run directory qemu is
          started in.
        '';
      };

      device = lib.mkOption {
        type = lib.types.str;
        default = "vm0";
        description = ''
          The id microvm.nix gives the guest's virtio-mem device.
        '';
      };
    };

    engine = lib.mkOption {
      type = lib.types.attrsOf lib.types.anything;
      default = { };
      example = {
        burst_reserve = "512MiB";
        open_guard = "384MiB";
      };
      description = ''
        What the scenario changes about the agent's engine settings.
      '';
    };
  };

  config = {
    system.stateVersion = lib.trivial.release;

    networking = {
      hostName = "pazari-harness";
      useNetworkd = true;
    };

    documentation.enable = false;

    services.journald.settings.Journal = {
      ForwardToConsole = "yes";
      TTYPath = "/dev/ttyS0";
      MaxLevelConsole = "info";
    };

    boot = {
      kernelParams = [ "quiet" ];
      consoleLogLevel = 3;
    };

    microvm = {
      hypervisor = "qemu";

      vcpu = 2;
      mem = 1536;
      hotplugMem = 4096;
      hotpluggedMem = 256;
      balloon = false;

      vsock.cid = 4242;

      interfaces = [
        {
          type = "user";
          id = "user";
          inherit mac;
        }
      ];

      forwardPorts = [
        {
          from = "host";
          proto = "tcp";
          host = {
            address = "127.0.0.1";
            port = cfg.forwardedPort;
          };
          guest = {
            inherit address;
            port = cfg.metricsPort;
          };
        }
      ];

      shares = [
        {
          source = "/nix/store";
          mountPoint = "/nix/.ro-store";
          tag = "ro-store";
          proto = "9p";
          readOnly = true;
        }
        {
          inherit (cfg.share) source mountPoint tag;
          proto = "9p";
        }
      ];

      qemu = {
        machine = "q35";

        extraArgs = [
          "-qmp"
          "unix:${cfg.qmp.socket},server=on,wait=off"
        ];
      };
    };

    systemd = {
      network.networks."10-user" = {
        matchConfig.MACAddress = mac;
        address = [ "${address}/24" ];
        routes = [
          {
            Destination = "0.0.0.0/0";
            Gateway = gateway;
          }
        ];
      };

      services = {
        "serial-getty@ttyS0".enable = false;

        ${cfg.workload.unit} = {
          description = "pazari harness workload";
          wantedBy = [ "multi-user.target" ];
          wants = [ "pazari-agent.service" ];
          after = [
            "pazari-agent.service"
            cfg.workload.slice
          ];

          unitConfig.RequiresMountsFor = cfg.share.mountPoint;

          serviceConfig = {
            ExecStartPre = "${release} ${cfg.share.mountPoint}/${cfg.workload.gate}";
            ExecStart = lib.escapeShellArgs ([ stress ] ++ cfg.workload.arguments ++ cfg.workload.phases);
            TimeoutStartSec = 300;
            Slice = cfg.workload.slice;
            StandardOutput = "file:${cfg.share.mountPoint}/${cfg.workload.log}";
            StandardError = "journal";
            SyslogIdentifier = cfg.workload.unit;
            Restart = "no";
          };
        };
      };
    };

    services.pazari-agent = {
      enable = true;

      inherit (cfg.workload) slice;
      inherit (cfg) metricsPort;

      openFirewall = true;
      systemMemoryMin = "192M";

      settings = {
        tick_interval = "500ms";

        engine = engine // cfg.engine;

        runtime.boot_mem = "1536MiB";

        transport = {
          kind = "vsock";
          cid = 2;
          port = cfg.vsockPort;
        };
      };
    };
  };
}
