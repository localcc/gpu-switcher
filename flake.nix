{
  description = "GPU switching utility";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      flake-utils,
      nixpkgs,
    }:
    {
      nixosModules.default =
        { pkgs, lib, ... }:
        {
          imports = [ ./nix/module.nix ];
          services.gpu-switcher.package =
            lib.mkDefault
              self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        };
    }
    // (import ./package.nix { inherit self flake-utils nixpkgs; });
}
