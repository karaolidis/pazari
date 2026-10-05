{ pkgs, ... }: {
  projectRootFile = "flake.nix";

  programs = {
    nixfmt = {
      enable = true;
      strict = true;
    };

    rustfmt.enable = true;
    shellcheck.enable = true;

    ruff = {
      format = true;
      check = true;
    };

    mandoc = {
      enable = true;
      level = "style";
      manpath = [ "man" ];
      manuals = with pkgs; [
        man-pages
        systemd
        qemu_kvm
      ];
    };
  };

  settings.global.excludes = [ ".envrc" ];
}
