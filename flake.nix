{
  description = "Muninn: your knowledge vault, reachable from anywhere";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
        muninn = pkgs.rustPlatform.buildRustPackage {
          pname = "muninn";
          version = cargoToml.workspace.package.version;
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;
          cargoTestFlags = [ "--workspace" ];
          meta = {
            description = "Your knowledge vault, reachable from anywhere";
            mainProgram = "muninn";
          };
        };
      in
      {
        packages.default = muninn;
        packages.muninn = muninn;

        apps.default = flake-utils.lib.mkApp { drv = muninn; };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            rustc
            cargo
            clippy
            rustfmt
            rust-analyzer
            tmux
            vhs # terminal recordings for the README and posts (demo/*.tape)
          ];
          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
        };
      });
}
