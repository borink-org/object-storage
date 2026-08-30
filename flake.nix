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
        # cargo-c on its own, for a build that cannot run inside the shell
        # below. CI's bare-metal job is the one that needs this.
        inherit (pkgs) cargo-c;

        # The C library as another derivation can depend on it: the archive,
        # the header, and the pkg-config file that points at both. A consumer
        # puts this in `buildInputs` and needs no `propagatedBuildInputs`.
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "borink-object-storage-c";
          version = "0.0.0";
          src = self;

          # The lock file rather than a vendor hash.
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = [ pkgs.cargo-c ];

          # These replace `buildRustPackage`'s own phases rather than
          # appending to them, so the crate compiles once. `--libdir` puts the
          # pkg-config file where nixpkgs' setup hook looks. docs/PACKAGING.md
          # covers both.
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

          # CI runs the workspace's tests; `checks` below tests what this
          # derivation produces.
          doCheck = false;
        };
      });

      # A consumer that knows only what pkg-config reported: no path into this
      # source tree and no store path written down.
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
          # Compares every struct's layout as the C compiler computes it
          # against what the archive was built with.
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
          # image with. Neither that toolchain nor this nixpkgs' rustc carries
          # a bare-metal Rust target. The archive it links comes from a
          # `cargo cbuild --target thumbv7em-none-eabihf` made outside this
          # shell.
          #
          # cbindgen is a tool here rather than a build-dependency of the C ABI
          # crate, so nothing a consumer vendors carries it and no build of
          # ours writes the header as a side effect. This pins its version the
          # same way the rest of this list is pinned.
          #
          # cargo-c builds and installs the C artifacts. Every consumer goes
          # through it; docs/PACKAGING.md says why.
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
