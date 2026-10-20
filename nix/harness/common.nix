{
  lib,
  pkgs,
  nixosSystem,
  nixosModules,
  microvmModule,
}:
let
  daemonPort = 19650;

  files = lib.filterAttrs (name: kind: kind == "regular" && lib.hasSuffix ".nix" name) (
    builtins.readDir ./scenarios
  );

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
    scenario:
    pkgs.runCommandLocal "pazari-harness-${scenario.name}-assertions"
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
in
rec {
  inherit
    assertions
    daemonPort
    driver
    python
    ;

  json = pkgs.formats.json { };

  scenarios = lib.mapAttrs' (
    file: _:
    let
      name = lib.removeSuffix ".nix" file;
    in
    lib.nameValuePair name (import (./scenarios + "/${file}") // { inherit name; })
  ) files;

  machines =
    scenario: extra:
    lib.listToAttrs (
      lib.imap0 (
        index: name:
        lib.nameValuePair name (nixosSystem {
          modules = [
            microvmModule
            nixosModules.guest
            ./guest.nix
            { services.pazari-agent.harness = { inherit index name; }; }
            (scenario.guests.${name}.module or { })
            (extra name)
            { nixpkgs.pkgs = pkgs; }
          ];
        })
      ) (lib.attrNames scenario.guests)
    );

  settings =
    scenario: built: qmp:
    lib.recursiveUpdate {
      tick.interval = "500ms";
      status.interval = "500ms";
      actuation.deadline = "30s";

      metrics.listen = "127.0.0.1:${toString daemonPort}";

      guests = lib.mapAttrs (
        name: guest:
        {
          transport = {
            kind = "vsock";
            inherit (built.${name}.config.services.pazari-agent.harness) cid;
            port = built.${name}.config.services.pazari-agent.harness.vsockPort;
          };

          qmp = {
            path = qmp name;
            inherit (built.${name}.config.services.pazari-agent.harness.qmp) device;
            deadline = "5s";
            mode.kind = "held";
          };
        }
        // (guest.daemon or { })
      ) scenario.guests;
    } (scenario.daemon.settings or { });

  entry = scenario: built: parts: {
    inherit (scenario) name description timeout;

    breaches = scenario.breaches or false;
    sample = scenario.sample or 0.25;
    assertions = "${assertions scenario}/assertions.py";

    inherit (parts) daemon;

    guests = lib.mapAttrs (
      guestName: _:
      let
        harness = built.${guestName}.config.services.pazari-agent.harness;
      in
      {
        port = harness.forwardedPort;
        share = harness.share.source;
        control = harness.control;

        inherit (harness.workload) log gate;
      }
      // (parts.perGuest or (_: _: { })) guestName harness
    ) scenario.guests;
  };
}
