{
  description = "mx-keypad - drive the Logitech MX Creative Keypad's LCD keys from a JSON setup";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "aarch64-darwin"
        "x86_64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      # Single source of truth for the version: Cargo.toml.
      version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
    in
    {
      packages = forAllSystems (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "mx-keypad";
          inherit version;
          # Only what cargo consumes: icons are embedded at build time, and a
          # test validates examples/demo.json.
          src = nixpkgs.lib.fileset.toSource {
            root = ./.;
            fileset = nixpkgs.lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./src
              ./icons
              ./examples
            ];
          };
          cargoLock.lockFile = ./Cargo.lock;
          meta = {
            description = "Drive the Logitech MX Creative Keypad's LCD keys from a JSON setup";
            license = with pkgs.lib.licenses; [ mit asl20 ];
            mainProgram = "mx-keypad";
          };
        };
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            rust-analyzer
            clippy
            rustfmt
          ];
        };
      });
    };
}
