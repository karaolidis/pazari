{
  lib,
  pkgs,
  nixosSystem,
  nixosModules,
  microvmModule,
}:
let
  toml = pkgs.formats.toml { };
  json = pkgs.formats.json { };

  daemonPort = 19650;

  files = lib.filterAttrs (name: kind: kind == "regular" && lib.hasSuffix ".nix" name) (
    builtins.readDir ./scenarios
  );

  scenarios = lib.mapAttrs' (
    file: _: lib.nameValuePair (lib.removeSuffix ".nix" file) (import (./scenarios + "/${file}"))
  ) files;

  machines = lib.mapAttrs (
    _: scenario:
    (nixosSystem {
      modules = [
        microvmModule
        nixosModules.guest
        ./guest.nix
        scenario.guest
        { nixpkgs.pkgs = pkgs; }
      ];
    }).config
  ) scenarios;

  settings = name: scenario: {
    tick_interval = "500ms";

    policy = {
      welcome_status_interval = "500ms";
      reconcile_deadline = "30s";
    }
    // scenario.daemon.policy;

    metrics.listen = "127.0.0.1:${toString daemonPort}";

    guests.${name} = {
      transport = {
        kind = "vsock";
        inherit (machines.${name}.microvm.vsock) cid;
        port = machines.${name}.pazari.harness.vsockPort;
      };

      qmp = {
        path = "@QMP_SOCKET@";
        inherit (machines.${name}.pazari.harness.qmp) device;
        deadline = "5s";
        mode.kind = "held";
      };
    }
    // scenario.daemon.guest;
  };

  python = pkgs.python3.withPackages (
    pkgs: with pkgs; [
      matplotlib
      prometheus-client
    ]
  );

  driver = pkgs.runCommandLocal "pazari-harness-driver" { } ''
    mkdir -p "$out/lib"
    cp -r ${./driver} "$out/lib/pazari_harness"
  '';

  assertions =
    name: scenario:
    pkgs.runCommandLocal "pazari-harness-${name}-assertions"
      {
        inherit (scenario) assertions;
        passAsFile = [ "assertions" ];
      }
      ''
        mkdir -p "$out"
        cp "$assertionsPath" "$out/assertions.py"

        PYTHONPATH=${driver}/lib ${python}/bin/python3 -m py_compile "$out/assertions.py"
        rm -rf "$out/__pycache__"
      '';

  manifest = json.generate "pazari-harness.json" {
    pazarid = lib.getExe' pkgs.pazari "pazarid";

    scenarios = lib.mapAttrs (
      name: scenario:
      let
        harness = machines.${name}.pazari.harness;
      in
      {
        inherit (scenario) description timeout;

        guest = name;
        runner = "${machines.${name}.microvm.declaredRunner}";
        settings = "${toml.generate "pazarid-${name}.toml" (settings name scenario)}";
        assertions = "${assertions name scenario}/assertions.py";

        share = harness.share.source;
        log = harness.workload.log;
        gate = harness.workload.gate;
        qmp = harness.qmp.socket;

        ports = {
          agent = harness.forwardedPort;
          daemon = daemonPort;
        };
      }
    ) scenarios;
  };
in
{
  inherit machines;

  app = pkgs.writeShellApplication {
    name = "pazari-harness";

    runtimeInputs = [ python ];

    text = ''
      export PYTHONPATH=${driver}/lib''${PYTHONPATH:+:$PYTHONPATH}
      export PYTHONDONTWRITEBYTECODE=1

      exec python3 -m pazari_harness --manifest ${manifest} "$@"
    '';
  };
}
