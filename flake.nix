{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, ... }:
    let
      inherit (nixpkgs) lib;
      pkgs = nixpkgs.legacyPackages.x86_64-linux;
    in
    {
      packages.x86_64-linux.default = pkgs.rustPlatform.buildRustPackage {
        pname = "nv";
        version = (lib.importTOML ./Cargo.toml).package.version;

        src = lib.fileset.toSource {
          root = ./.;
          fileset = lib.fileset.unions [
            ./Cargo.toml
            ./Cargo.lock
            ./src
            ./tests
          ];
        };
        cargoLock.lockFile = ./Cargo.lock;

        nativeBuildInputs = [ pkgs.makeWrapper ];
        nativeCheckInputs = [ pkgs.jq ];
        postInstall = ''
          wrapProgram $out/bin/nv --prefix PATH : ${
            lib.makeBinPath (
              with pkgs;
              [
                coreutils
                curl
                gnutar
                jq
              ]
            )
          }
        '';

        meta = {
          description = "Minimal Neovim version manager for the official Linux builds";
          homepage = "https://github.com/ammix/nv";
          mainProgram = "nv";
        };
      };
    };
}
