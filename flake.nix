{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, ... }:
    let
      inherit (nixpkgs) lib;
      pkgs = nixpkgs.legacyPackages.x86_64-linux;
    in
    {
      packages.x86_64-linux.default = pkgs.buildGoModule {
        pname = "nv";
        version = "0.1.0";

        src = lib.fileset.toSource {
          root = ./.;
          fileset = lib.fileset.unions [
            ./go.mod
            (lib.fileset.fileFilter (file: file.hasExt "go") ./.)
          ];
        };
        vendorHash = null;

        env.CGO_ENABLED = 0;
        ldflags = [
          "-s"
          "-w"
        ];

        meta = {
          description = "Minimal Neovim version manager for the official Linux builds";
          homepage = "https://github.com/ammix/nv";
          mainProgram = "nv";
        };
      };
    };
}
