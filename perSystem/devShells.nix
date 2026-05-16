{
  perSystem = {
    config,
    pkgs,
    inputs',
    ...
  }: let
    toolchain = with inputs'.fenix.packages;
      combine [
        stable.rustc
        stable.cargo
        stable.clippy
        stable.rustfmt
        stable.rust-analyzer
      ];
  in {
    devShells.default = with pkgs;
      mkShell {
        packages = [
          toolchain
          cmake
          pkg-config
          openssl
          zlib
          config.treefmt.build.wrapper
        ];
      };
  };
}
