{
  self,
  flake-utils,
  nixpkgs,
}:
let
  platforms = [ "x86_64-linux" ];
in
flake-utils.lib.eachSystem platforms (
  system:
  let
    pkgs = nixpkgs.legacyPackages.${system};
  in
  {
    packages = {
      gpu-switcher = pkgs.rustPlatform.buildRustPackage {
        pname = "gpu-switcher";
        version = "0.1.0";

        src = ./.;

        cargoLock = {
          lockFile = ./Cargo.lock;
        };

        nativeBuildInputs = with pkgs; [
          pkg-config
        ];

        buildInputs = with pkgs; [
          systemd
          kmod
        ];

        doCheck = false;

        postPatch = ''
          substituteInPlace switcherd/src/main.rs --replace 'Command::new("modprobe")' 'Command::new("${pkgs.lib.getExe' pkgs.kmod "modprobe"}")'
          substituteInPlace switcherd/src/main.rs --replace 'Command::new("rmmod")' 'Command::new("${pkgs.lib.getExe' pkgs.kmod "rmmod"}")'
        '';

        postInstall = ''
          install -Dm444 -t $out/udev/rules.d/ udev/*.rules
          install -Dm444 -t $out/share/dbus-1/system.d/ dbus/cc.localcc.GpuSwitcher.conf
        '';

        meta = with pkgs.lib; {
          inherit platforms;
          description = "Dedicated GPU switcher";
          homepage = "https://github.com/localcc/gpu-switcher";
          license = licenses.gpl3Only;
        };
      };

      default = self.packages.${system}.gpu-switcher;
    };

    devShells.default = pkgs.mkShell {
      inputsFrom = [ self.packages.${system}.gpu-switcher ];
      packages = with pkgs; [
        rust-analyzer
        rustfmt
        clippy
        gdb
      ];
    };
  }
)
