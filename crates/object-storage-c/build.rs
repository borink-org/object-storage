//! Writes the C header that declares this crate's ABI.
//!
//! `cbindgen` reads `src/lib.rs` and generates `include/borink/object_storage.h`.
//! The header is committed, so a change to the ABI shows up in the diff and CI
//! fails when the two disagree.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=cbindgen.toml");

    let crate_directory = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let configuration = cbindgen::Config::from_file(crate_directory.join("cbindgen.toml"))
        .expect("cbindgen.toml is readable");
    let generated = match cbindgen::Builder::new()
        .with_crate(&crate_directory)
        .with_config(configuration)
        .generate()
    {
        Ok(generated) => generated,
        Err(failure) => {
            // A crate that cannot be parsed is a build failure everywhere else
            // in this workspace, so it is one here too.
            panic!("cbindgen could not read this crate: {failure}");
        }
    };

    let out_directory = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    generated.write_to_file(out_directory.join("object_storage.h"));

    // Rewrite the committed header only when it changed. Writing it every time
    // would touch its timestamp on every build.
    let committed = crate_directory.join("include/borink/object_storage.h");
    let fresh = std::fs::read_to_string(out_directory.join("object_storage.h")).unwrap();
    if std::fs::read_to_string(&committed).ok().as_deref() != Some(fresh.as_str())
        && let Err(failure) = std::fs::write(&committed, &fresh)
    {
        println!("cargo:warning=the committed header could not be written: {failure}");
    }
}
