{
  lib,
  pkgs,
  nixosSystem,
  nixosModules,
  microvmModule,
  microvmHostModule,
  runNixOSTest,
}:
let
  common = import ./common.nix {
    inherit
      lib
      pkgs
      nixosSystem
      nixosModules
      microvmModule
      ;
  };

  nested = import ./nested.nix {
    inherit
      lib
      pkgs
      common
      nixosModules
      microvmHostModule
      runNixOSTest
      ;

    inherit (common) scenarios;
  };

  measured = name: lib.hasPrefix "measure-" name;
in
{
  tests = lib.filterAttrs (name: _: !measured name) nested.tests;
  measurements = lib.filterAttrs (name: _: measured name) nested.tests;
}
