{
  lib,
  pkgs,
  config,
  ...
}:
let
  cfg = config.services.gpu-switcher;
  json = pkgs.formats.json { };

  hotplugType =
    with lib;
    types.enum [
      "Normal"
      "Asus"
    ];

  options = with lib; {
    options = {
      device_path = mkOption {
        type = types.str;
        description = ''
          GPU device path.

          Example: 0000:64:00.0
        '';
      };
      hotplug_type = mkOption {
        type = hotplugType;
        description = ''
          GPU Hotplug mechanism to use.
        '';
        default = "Normal";
      };
    };
  };
in
{
  options = {
    services.gpu-switcher = {
      enable = lib.mkEnableOption "Enable the gpu-switcher service";

      package = lib.mkOption {
        type = lib.types.package;
        description = "The gpu-switcher package to use";
      };

      settings = lib.mkOption {
        type = lib.types.nullOr (lib.types.submodule options);
        description = "gpu-switcher settings";
      };
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];

    environment.etc."gpu-switcherd.conf" = {
      source = json.generate "gpu-switcherd.conf" cfg.settings;
      mode = "0644";
    };

    services.dbus.enable = true;

    systemd.services.gpu-switcher = {
      description = "Dedicated GPU switcher";
      before = [
        "graphical.target"
        "multi-user.target"
        "display-manager.service"
        "nvidia-powerd.service"
      ];
      wantedBy = [ "multi-user.target" ];

      environment = {
        PATH = lib.mkForce null; 
      };

      unitConfig = {
        StartLimitInterval = 200;
        StartLimitBurst = 2;
      };

      serviceConfig = {
        ExecStart = lib.getExe' cfg.package "switcherd";
        Restart = "on-failure";
        RestartSec = 1;
        Type = "dbus";
        BusName = "cc.localcc.GpuSwitcher";
      };
    };
  };
}
