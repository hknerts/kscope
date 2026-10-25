{
  description = "kscope: a fast, read-only Kubernetes TUI for logs, metrics and events";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
    in
    {
      packages = forAllSystems (pkgs: {
        kscope = pkgs.rustPlatform.buildRustPackage {
          pname = "kscope";
          inherit (cargoToml.package) version;

          # Only what the build reads, so editing docs or packaging does not
          # rebuild the package.
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./src
              ./tests
            ];
          };
          cargoLock.lockFile = ./Cargo.lock;

          # kube uses rustls with ring: no OpenSSL, no system libraries.
          # The test suite needs no cluster, so it runs as the check phase.

          meta = {
            description = cargoToml.package.description;
            homepage = "https://github.com/hknerts/kscope";
            license = pkgs.lib.licenses.mit;
            mainProgram = "kscope";
            platforms = pkgs.lib.platforms.unix;
          };
        };
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.kscope;
      });

      apps = forAllSystems (pkgs: {
        default = {
          type = "app";
          program = pkgs.lib.getExe self.packages.${pkgs.stdenv.hostPlatform.system}.kscope;
        };
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${pkgs.stdenv.hostPlatform.system}.kscope ];
          packages = with pkgs; [
            clippy
            rustfmt
            rust-analyzer
            cargo-deny
            kind
            kubectl
          ];
        };
      });

      formatter = forAllSystems (pkgs: pkgs.nixpkgs-fmt);
    };
}
