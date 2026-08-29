//! Compiles the generated bridge and puts it in the archive that C++ links.
//!
//! A host is built by CMake. Only the glue that `cxx` writes
//! is compiled here, so it is part of the same archive as the Rust code it
//! calls.

fn main() {
    cxx_build::bridge("src/lib.rs")
        .std("c++23")
        .compile("borink-object-storage-cxx");

    println!("cargo:rerun-if-changed=src/lib.rs");
}
