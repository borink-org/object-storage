{
  description = "Development environment for the C++ host of borink-object-storage";

  inputs = {
    # Pinned to a revision, not to a channel, so every developer and the CI
    # job compile the C++ hosts against the same libcurl, Boost and OpenSSL.
    nixpkgs.url = "github:nixos/nixpkgs/a5cbcfe954791221bfffe2307f7d1a1bf61a871e";
  };

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forEachSystem = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      devShells = forEachSystem (pkgs: {
        default = pkgs.mkShell {
          # The Rust crates need only cargo. The rest is what the two C and
          # C++ checks build and link: CMake drives the libcurl host, libcurl
          # is what it sends with, and arm-none-eabi-gcc is what
          # `crates/object-storage-c/tests/freestanding.sh` links a board
          # image with. That toolchain carries no Rust target: the archive it
          # links comes from a `cargo build --target thumbv7em-none-eabihf`
          # made outside this shell.
          nativeBuildInputs = [
            pkgs.cargo
            pkgs.rustc
            pkgs.clippy
            pkgs.rustfmt
            pkgs.cmake
            pkgs.pkg-config
            pkgs.gcc-arm-embedded
          ];
          buildInputs = [
            pkgs.curl.dev
            # Verifying a certificate needs the trust store, which a shell
            # does not carry on its own.
            pkgs.cacert
          ];

          # An unoptimized build cannot use _FORTIFY_SOURCE, and the default
          # hardening flags of this shell would warn on every file.
          hardeningDisable = [ "fortify" ];
        };
      });
    };
}
