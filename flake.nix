{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    lib = {
      # FIXME: https://github.com/NixOS/nix/issues/12281
      url = "git+https://git.karaolidis.com/karaolidis/nix-lib.git";
      inputs = {
        nixpkgs.follows = "nixpkgs";
        treefmt-nix.follows = "treefmt-nix";
      };
    };

    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    microvm = {
      url = "github:microvm-nix/microvm.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs:
    {
      overlays.default =
        final: prev:
        let
          pkgs = final;
        in
        {
          pazari = pkgs.callPackage ./nix/package.nix { };
        };

      nixosModules = {
        guest = ./nix/modules/guest.nix;
        host = ./nix/modules/host.nix;

        default.imports = [
          inputs.self.nixosModules.guest
          inputs.self.nixosModules.host
        ];
      };
    }
    // (
      let
        system = "x86_64-linux";

        pkgs = import inputs.nixpkgs {
          inherit system;
          config.allowUnfree = true;
          overlays = [
            inputs.lib.overlays.default
            inputs.self.overlays.default
          ];
        };

        treefmt = inputs.treefmt-nix.lib.evalModule pkgs ./treefmt.nix;

        harness = import ./nix/harness {
          inherit pkgs;
          inherit (pkgs) lib;
          inherit (pkgs.testers) runNixOSTest;
          inherit (inputs.nixpkgs.lib) nixosSystem;
          inherit (inputs.self) nixosModules;
          microvmHostModule = inputs.microvm.nixosModules.host;
          microvmModule = inputs.microvm.nixosModules.microvm;
        };
      in
      {
        devShells.${system}.default = pkgs.mkShell {
          packages = with pkgs; [
            inputs.fenix.packages.${system}.latest.toolchain
            cargo-udeps
            cargo-outdated
            cargo-edit
            cargo-expand
            mandoc
            (python3.withPackages (
              pkgs: with pkgs; [
                matplotlib
                prometheus-client
              ]
            ))
          ];

          shellHook = ''
            TOP="$(git rev-parse --show-toplevel)"
            export CARGO_HOME="$TOP/.cargo"
          '';
        };

        packages.${system} = with pkgs; {
          default = pazari;
          inherit pazari;
        };

        legacyPackages.${system} = harness.measurements;

        formatter.${system} = treefmt.config.build.wrapper;

        checks.${system} =
          let
            packages = pkgs.lib.mapAttrs' (
              name: pkgs.lib.nameValuePair "package-${name}"
            ) inputs.self.packages.${system};

            devShells = pkgs.lib.mapAttrs' (
              name: pkgs.lib.nameValuePair "devShell-${name}"
            ) inputs.self.devShells.${system};

            formatter.formatting = treefmt.config.build.check inputs.self;

            tests = pkgs.lib.mapAttrs' (name: pkgs.lib.nameValuePair "test-${name}") harness.tests;

            modules = import ./nix/checks/modules.nix {
              inherit pkgs;
              inherit (pkgs) lib;
              inherit (inputs.nixpkgs.lib) nixosSystem;
              inherit (inputs.self) nixosModules;
            };
          in
          packages // devShells // formatter // modules // tests;
      }
    );
}
