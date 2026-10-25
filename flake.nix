{
  description = "Rill, a typed scripting language for Unix automation";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "rill";
          version = cargoToml.package.version;
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
          # The integration tests spawn processes and ssh; run them with
          # `cargo test` in the dev shell instead of inside the sandbox.
          doCheck = false;
          meta = with pkgs.lib; {
            description = "A typed scripting language for Unix automation";
            homepage = "https://github.com/Kirth/Rill";
            license = with licenses; [ mit asl20 ];
            mainProgram = "rill";
          };
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            rustfmt
            clippy
            rust-analyzer
            cargo-insta
            just
            # `just test-sql` runs a throwaway server for tests/sql.rs.
            postgresql
          ];
          RUST_BACKTRACE = "1";
        };
      });
}
