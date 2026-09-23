{
  lib,
  pkgs,
  common,
  nixosModules,
  microvmHostModule,
  runNixOSTest,
  scenarios,
}:
let
  inherit (common)
    daemonPort
    driver
    entry
    json
    machines
    python
    settings
    ;

  stateDir = "/var/lib/microvms";
  runDirectory = "/var/lib/pazari-harness";

  slack = 2048;

  test =
    name: scenario:
    let
      built = machines scenario (guest: {
        services.pazari-agent.harness.share.source = "${runDirectory}/${guest}/share";
      });

      guests = lib.attrNames scenario.guests;

      manifest = json.generate "pazari-harness-${name}.json" {
        scenario = entry scenario built {
          perGuest = guest: _: { unit = "microvm@${guest}.service"; };

          daemon = {
            port = daemonPort;
            unit = "pazarid.service";
          };
        };
      };

      runner = pkgs.writeShellApplication {
        name = "pazari-harness-nested";

        runtimeInputs = [
          python
          pkgs.systemd
        ];

        text = ''
          export PYTHONPATH=${driver}/lib''${PYTHONPATH:+:$PYTHONPATH}
          export PYTHONDONTWRITEBYTECODE=1
          export MPLCONFIGDIR=${runDirectory}/.matplotlib

          exec python3 -m pazari_harness.nested \
            --manifest ${manifest} \
            --directory ${runDirectory} \
            "$@"
        '';
      };

      held = lib.foldl' (
        total: guest: total + guest.config.microvm.mem + guest.config.microvm.hotplugMem
      ) 0 (lib.attrValues built);

      budget = scenario.timeout + 300;
    in
    runNixOSTest {
      name = "pazari-${name}";

      meta.timeout = budget + 1200;

      nodes.host = {
        imports = [
          microvmHostModule
          nixosModules.host
        ]
        ++ lib.optional (scenario ? host) scenario.host;

        virtualisation = {
          cores = 2 + 2 * lib.length guests;
          memorySize = slack + held;
          diskSize = 4096;

          qemu.options = [
            "-cpu"
            "host"
          ];
        };

        boot = {
          kernelModules = [ "vhost_vsock" ];
          kernelParams = [ "psi=1" ];
        };

        environment.systemPackages = [ runner ];

        microvm.vms = lib.mapAttrs (guest: _: { evaluatedConfig = built.${guest}; }) scenario.guests;

        systemd.services."microvm@".serviceConfig.UMask = "0002";

        services.pazarid = {
          enable = true;
          settings = settings scenario built (guest: "${stateDir}/${guest}/qmp.sock");
        };
      };

      testScript = ''
        from datetime import timedelta

        guests = ${builtins.toJSON guests}

        host.wait_for_unit("multi-user.target")
        host.succeed("test -c /dev/kvm")
        host.wait_for_unit("pazarid.service")

        for guest in guests:
            host.wait_for_unit(
                "microvm@" + guest + ".service", timeout = timedelta(minutes = 10)
            )

        code, output = host.execute(
            "pazari-harness-nested 2>&1",
            timeout = timedelta(seconds = ${toString budget}),
        )
        print(output)

        if code != 0:
            for stream in ["pazarid", *guests]:
                print(
                    host.execute(
                        "tail -n 120 ${runDirectory}/logs/" + stream + ".log"
                    )[1]
                )

            print(host.execute("journalctl -b --no-pager -n 200")[1])

            raise Exception("${name} did not hold")

        host.copy_from_machine("${runDirectory}")
      '';
    };
in
{
  tests = lib.mapAttrs test scenarios;
}
