{ rustPlatform }:
rustPlatform.buildRustPackage {
  pname = "pazari";
  version = "0.1.0";

  src = ../.;
  cargoLock.lockFile = ../Cargo.lock;
}
