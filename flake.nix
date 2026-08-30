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
      # The systems this shell is built and tested on. Every workflow runs on
      # ubuntu-latest, so darwin was never exercised; add it back when
      # something builds there.
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forEachSystem = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forEachSystem (pkgs: {
        # cargo-c on its own, from the same pinned nixpkgs as the shell below.
        # The bare-metal build in CI needs a rustc that carries the board
        # target, which this nixpkgs' rustc does not, so that job puts this on
        # PATH beside its own toolchain instead of running inside the shell.
        inherit (pkgs) cargo-c;

        # The C library as something another derivation can depend on: the
        # archive, the header and the pkg-config file that points at both.
        # `cargo cinstall` writes all three; cargo on its own installs
        # nothing, which is the gap cargo-c exists to fill.
        #
        # Nothing downstream needs `propagatedBuildInputs`. The pkg-config
        # file has no `Requires` and no `Requires.private`, because the only
        # dependencies are Rust crates linked into the archive and the public
        # header includes nothing but <stdbool.h>, <stddef.h> and <stdint.h>.
        # A consumer needs this package in `buildInputs` and nothing else.
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "borink-object-storage-c";
          version = "0.0.0";
          src = self;

          # The lock file rather than a vendor hash, so adding a dependency
          # does not also mean chasing a hash that nothing else checks.
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = [ pkgs.cargo-c ];

          # `buildRustPackage` would otherwise `cargo build` the workspace and
          # leave cbuild to compile it a second time. These two phases replace
          # that, so the crate is built once.
          #
          # cargo-c defaults libdir to a multiarch subdirectory. nixpkgs' setup
          # hook puts `lib/pkgconfig` on `PKG_CONFIG_PATH` and nothing below
          # it, so the flat `$out/lib` is what makes this package findable.
          buildPhase = ''
            runHook preBuild
            ${pkgs.buildPackages.rust.envVars.setEnv} cargo cbuild \
              --package borink-object-storage-c \
              --release --frozen --library-type staticlib \
              --prefix=${placeholder "out"} --libdir=${placeholder "out"}/lib \
              --target ${pkgs.stdenv.hostPlatform.rust.rustcTarget}
            runHook postBuild
          '';

          installPhase = ''
            runHook preInstall
            ${pkgs.buildPackages.rust.envVars.setEnv} cargo cinstall \
              --package borink-object-storage-c \
              --release --frozen --library-type staticlib \
              --prefix=${placeholder "out"} --libdir=${placeholder "out"}/lib \
              --target ${pkgs.stdenv.hostPlatform.rust.rustcTarget}
            runHook postInstall
          '';

          # The workspace's tests run in CI against the Rust source. This
          # derivation exists to package the C artifacts, and `checks` below
          # is what tests them.
          doCheck = false;
        };
      });

      # A consumer that knows only what pkg-config told it: no path into this
      # source tree, no store path written down, no propagation. This is the
      # packaged path the CMake project and CI cannot exercise, because both
      # of those build from a checkout.
      checks = forEachSystem (pkgs: {
        pkg-config-consumer = pkgs.stdenv.mkDerivation {
          name = "borink-object-storage-c-pkg-config-consumer";
          src = ./crates/object-storage-c/tests;
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [ self.packages.${pkgs.stdenv.hostPlatform.system}.default ];
          buildPhase = ''
            runHook preBuild
            $CC -Wall -Wextra -Wpedantic -std=c11 abi.c \
              $(pkg-config --cflags borink-object-storage-c) \
              $(pkg-config --static --libs borink-object-storage-c) \
              -o abi_test
            runHook postBuild
          '';
          # The ABI test compares every struct's layout as the C compiler
          # computes it against what the archive was built with.
          doCheck = true;
          checkPhase = ''
            runHook preCheck
            ./abi_test
            runHook postCheck
          '';
          installPhase = ''
            runHook preInstall
            install -Dm755 abi_test $out/bin/abi_test
            runHook postInstall
          '';
        };
      });

      devShells = forEachSystem (pkgs: {
        default = pkgs.mkShell {
          # The Rust crates need only cargo. The rest is what the C and C++
          # checks build and link: CMake drives the libcurl host, libcurl is
          # what it sends with, and arm-none-eabi-gcc is what
          # `crates/object-storage-c/tests/freestanding.sh` links a board
          # image with. That toolchain carries no Rust target, and this
          # nixpkgs' rustc carries no bare-metal one: the archive it links
          # comes from a `cargo cbuild --target thumbv7em-none-eabihf` made
          # outside this shell.
          #
          # cbindgen is a tool here rather than a build-dependency of the C ABI
          # crate, so nothing a consumer vendors carries it and no build of
          # ours writes the header as a side effect. This pins its version the
          # same way the rest of this list is pinned.
          #
          # cargo-c is how the C artifacts are produced, not a convenience:
          # `cargo cbuild` is what the CMake project and CI build the archive
          # with, and `cargo cinstall` is what writes the archive, the header
          # and the pkg-config file that a packaged consumer finds. Cargo
          # installs none of that on its own.
          nativeBuildInputs = [
            pkgs.cargo
            pkgs.rustc
            pkgs.clippy
            pkgs.rustfmt
            pkgs.cmake
            pkgs.pkg-config
            pkgs.gcc-arm-embedded
            pkgs.rust-cbindgen
            pkgs.cargo-c
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
