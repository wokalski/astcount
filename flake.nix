{
  description = "deslop: a fast, polyglot AST node counter";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      pkgsFor = system: import nixpkgs { inherit system; };
      npmPlatforms = {
        x86_64-linux = "linux-x64-gnu";
        aarch64-linux = "linux-arm64-gnu";
        x86_64-darwin = "darwin-x64";
        aarch64-darwin = "darwin-arm64";
      };
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "deslop";
            version = "0.1.0";
            src = pkgs.lib.fileset.toSource {
              root = ./.;
              fileset = pkgs.lib.fileset.unions [
                ./Cargo.toml
                ./Cargo.lock
                ./LICENSE
                ./README.md
                ./src
                ./tests
              ];
            };
            cargoLock.lockFile = ./Cargo.lock;
            env.TSLP_OFFLINE = "1";
          };
        }
      );

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/deslop";
          meta.description = "Count syntax-tree nodes across a source tree";
        };
      });

      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              clang
              clippy
              nodejs_24
              rust-analyzer
              rustc
              rustfmt
            ];
            env.TSLP_OFFLINE = "1";
          };
        }
      );

      checks = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          package = self.packages.${system}.default;
          npmPlatform = npmPlatforms.${system};
        in
        {
          inherit (self.packages.${system}) default;

          npm =
            pkgs.runCommand "deslop-npm-smoke"
              {
                nativeBuildInputs = [ pkgs.nodejs_24 ];
              }
              ''
                cp -R ${./npm} npm
                cp -R ${./scripts} scripts
                cp ${./Cargo.toml} Cargo.toml
                chmod -R u+w npm
                mkdir -p npm/platforms/${npmPlatform}/bin
                cp ${package}/bin/deslop npm/platforms/${npmPlatform}/bin/deslop
                node scripts/release.mjs check 0.1.0
                test "$(node npm/deslop/bin/deslop.js --version)" = "deslop 0.1.0"
                touch "$out"
              '';
        }
      );
    };
}
