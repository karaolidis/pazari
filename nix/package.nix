{ lib, rustPlatform }:
rustPlatform.buildRustPackage {
  pname = "pazari";
  version = "0.1.0";

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../clippy.toml
      ../rustfmt.toml
      ../crates

      ../docs/agent.example.toml
      ../docs/pazarid.example.toml
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;
}
