{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.pazari-agent.harness;

  stress = lib.getExe' pkgs.pazari "pazari-stress";

  mac = "02:00:00:0d:11:01";
  address = "10.0.2.15";
  gateway = "10.0.2.2";

  release = pkgs.writeShellScript "pazari-harness-release" ''
    until [ -e "$1" ]; do
      ${lib.getExe' pkgs.coreutils "sleep"} 0.1
    done
  '';

  courier = pkgs.writeShellScript "pazari-harness-courier" ''
    control="$1"
    ${lib.getExe' pkgs.coreutils "mkdir"} -p "$control"

    while :; do
      for job in "$control"/*.cmd; do
        if [ ! -e "$job" ] || [ -e "''${job%.cmd}.done" ]; then
          continue
        fi

        stem="''${job%.cmd}"

        ${lib.getExe' pkgs.bash "bash"} "$job" > "$stem.out" 2>&1
        code=$?

        ${lib.getExe' pkgs.coreutils "printf"} '%s\n' "$code" > "$stem.part"
        ${lib.getExe' pkgs.coreutils "mv"} "$stem.part" "$stem.done"
      done

      ${lib.getExe' pkgs.coreutils "sleep"} 0.1
    done
  '';

in
{
  options.services.pazari-agent.harness = {
    name = lib.mkOption {
      type = lib.types.str;
      default = "main";
      description = ''
        What the scenario and the daemon call this guest.
      '';
    };

    index = lib.mkOption {
      type = lib.types.ints.unsigned;
      default = 0;
      description = ''
        Where this guest sits in the scenario's guest list.
      '';
    };

    control = lib.mkOption {
      type = lib.types.str;
      default = "control";
      description = ''
        The directory in the share the driver leaves commands for the guest in.
      '';
    };

    logLevel = lib.mkOption {
      type = lib.types.str;
      default = "debug";
      description = ''
        `RUST_LOG` for the agent and the workload under it.
      '';
    };

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
      default = 19651 + cfg.index;
      defaultText = lib.literalExpression "19651 + config.services.pazari-agent.harness.index";
      description = ''
        The loopback port on the host `metricsPort` is forwarded to.
      '';
    };

    cid = lib.mkOption {
      type = lib.types.ints.unsigned;
      default = 4242 + cfg.index;
      defaultText = lib.literalExpression "4242 + config.services.pazari-agent.harness.index";
      description = ''
        The guest's vsock context id.
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
  };

  config = {
    system.stateVersion = lib.trivial.release;

    networking = {
      hostName = "pazari-${cfg.name}";
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

      vcpu = lib.mkDefault 2;
      mem = lib.mkDefault 1536;
      hotplugMem = lib.mkDefault 4096;
      hotpluggedMem = lib.mkDefault 256;
      balloon = false;

      vsock.cid = cfg.cid;

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
      slices.system.sliceConfig.MemoryMin = "192M";

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

        pazari-harness-courier = {
          description = "pazari harness courier";
          wantedBy = [ "multi-user.target" ];

          path = [ config.system.path ];

          unitConfig.RequiresMountsFor = cfg.share.mountPoint;

          serviceConfig = {
            ExecStart = "${courier} ${cfg.share.mountPoint}/${cfg.control}";
            Slice = "system.slice";
            SyslogIdentifier = "pazari-harness-courier";
            Restart = "always";
            RestartSec = "1s";
          };
        };

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
            Environment = [ "RUST_LOG=${cfg.logLevel}" ];
            Restart = "no";
          };
        };
      };
    };

    services.pazari-agent = {
      enable = true;

      inherit (cfg.workload) slice;
      inherit (cfg) metricsPort logLevel;

      openFirewall = true;

      settings = {
        tick.interval = lib.mkDefault "500ms";

        workload = {
          service_reserve = lib.mkDefault "640MiB";
          burst_reserve = lib.mkDefault "256MiB";
        };

        host.transport = {
          kind = "vsock";
          cid = 2;
        };

        resize = {
          settle_grace = lib.mkDefault "5s";
          deny_backoff = lib.mkDefault "2s";
        };

        reclaim.swappiness = lib.mkDefault 0;

        grow = lib.mkDefault {
          kind = "exponential";
          step = "512MiB";
        };

        release = lib.mkDefault {
          kind = "titrate";
          step = "256MiB";
          after = 3;
          cooldown = "5s";
        };

        exhaustion.mode = lib.mkDefault {
          kind = "stall";
          timeout = "300s";
        };
      };
    };
  };
}
