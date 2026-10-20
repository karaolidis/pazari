{
  lib,
  installShellFiles,
  runCommand,
  rustPlatform,
  symlinkJoin,
}:
let
  inherit ((lib.importTOML ../Cargo.toml).workspace.package) version;

  unwrapped = rustPlatform.buildRustPackage {
    pname = "pazari-unwrapped";
    inherit version;

    src = lib.fileset.toSource {
      root = ../.;
      fileset = lib.fileset.unions [
        ../Cargo.toml
        ../Cargo.lock
        ../clippy.toml
        ../rustfmt.toml
        ../crates

        ../examples
      ];
    };

    cargoLock.lockFile = ../Cargo.lock;

    postInstall = ''
      install -Dm644 -t "$out/share/doc/pazari/examples" examples/agent.toml examples/pazarid.toml
    '';
  };

  manual =
    runCommand "pazari-man-${version}"
      {
        src = lib.fileset.toSource {
          root = ../man;
          fileset = ../man;
        };

        nativeBuildInputs = [ installShellFiles ];
      }
      ''
        installManPage "$src"/*.1 "$src"/*.5 "$src"/*.7
      '';
in
symlinkJoin {
  name = "pazari-${version}";
  inherit version;

  paths = [
    unwrapped
    manual
  ];

  meta = {
    description = "Guest-initiated VM memory negotiation";
    mainProgram = "pazarid";
  };
}
