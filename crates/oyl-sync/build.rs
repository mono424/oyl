//! Generates the sp00ky client's schema from Oyl Cloud's schema
//! (`schema/src/schema.surql`), so the app's live reads always match it.

use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let schema = manifest.join("../../schema/src/schema.surql");
    println!("cargo:rerun-if-changed={}", schema.display());
    let surql = std::fs::read_to_string(&schema)
        .unwrap_or_else(|err| panic!("reading {}: {err}", schema.display()));
    let code = spooky_core::codegen::generate_rust(&surql, &[])
        .unwrap_or_else(|err| panic!("generating the client schema: {err}"));
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("cloud_schema.rs");
    std::fs::write(&out, code).unwrap_or_else(|err| panic!("writing {}: {err}", out.display()));
}
