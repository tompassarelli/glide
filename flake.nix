{
  description = "Glide — touchpad motion detection daemon for kanata";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs, ... }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f {
        pkgs = nixpkgs.legacyPackages.${system};
      });
    in {
      packages = forAllSystems ({ pkgs }: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "glide";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
        };
      });

      nixosModules.default = { config, lib, pkgs, ... }:
        let
          cfg = config.services.glide;
        in {
          options.services.glide = {
            enable = lib.mkEnableOption "Glide touchpad motion detection daemon";

            package = lib.mkOption {
              type = lib.types.package;
              default = self.packages.${pkgs.system}.default;
              description = "The glide package to use";
            };

            device = lib.mkOption {
              type = lib.types.str;
              default = "/dev/input/by-path/platform-AMDI0010:03-event-mouse";
              description = "Touchpad evdev device path";
            };

            kanataAddress = lib.mkOption {
              type = lib.types.str;
              default = "127.0.0.1:7070";
              description = "Kanata TCP server address (ip:port)";
            };

            virtualKey = lib.mkOption {
              type = lib.types.str;
              default = "pad-touch";
              description = "Kanata virtual key name to press/release on activation";
            };

            motionThreshold = lib.mkOption {
              type = lib.types.int;
              default = 2;
              description = "Min Euclidean displacement (device abs units) per evdev report to count as motion";
            };

            minStreak = lib.mkOption {
              type = lib.types.int;
              default = 16;
              description = "Consecutive motion-positive samples to activate (~7ms each, 16 ≈ 112ms)";
            };
          };

          config = lib.mkIf cfg.enable {
            systemd.services.glide = {
              description = "Glide touchpad motion detection daemon";
              after = [ "kanata-main.service" ];
              wants = [ "kanata-main.service" ];
              wantedBy = [ "multi-user.target" ];

              serviceConfig = {
                ExecStart = lib.concatStringsSep " " [
                  "${cfg.package}/bin/glide"
                  "--device" cfg.device
                  "--kanata-address" cfg.kanataAddress
                  "--virtual-key" cfg.virtualKey
                  "--motion-threshold" (toString cfg.motionThreshold)
                  "--min-streak" (toString cfg.minStreak)
                ];
                Restart = "on-failure";
                RestartSec = 2;
                SupplementaryGroups = [ "input" ];
              };
            };
          };
        };
    };
}
