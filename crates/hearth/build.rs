//! Compiles the web UI (crates/hearth-web) to WebAssembly and leaves the result in OUT_DIR/ui,
//! where src/web.rs embeds it. A plain `cargo build` therefore yields the one binary.

use std::{env, path::PathBuf, process::Command};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace = manifest.parent().unwrap().parent().unwrap().to_path_buf();
    for watched in ["crates/hearth-web/src", "crates/hearth-web/Cargo.toml", "crates/hearth-api/src", "Cargo.lock"] {
        println!("cargo:rerun-if-changed={}", workspace.join(watched).display());
    }
    println!("cargo:rerun-if-changed=build.rs");
    // The update command needs to know which platform's build to fetch.
    println!("cargo:rustc-env=HEARTH_TARGET={}", env::var("TARGET").unwrap());

    let release = env::var("PROFILE").unwrap() == "release";
    let profile = if release { "wasm-release" } else { "wasm-dev" };
    // A separate target folder: the outer cargo holds the lock on its own.
    let target_dir = workspace.join("target").join("wasm-ui");

    let mut cargo = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cargo.current_dir(&workspace).args([
        "build", "-p", "hearth-web", "--lib", "--target", "wasm32-unknown-unknown", "--profile", profile, "--target-dir",
    ]);
    cargo.arg(&target_dir);
    // The outer build's flags and wrappers (clippy, for one) must not leak into this one.
    for var in [
        "CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS", "RUSTDOCFLAGS", "RUSTC", "RUSTDOC", "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_TARGET", "CARGO_TARGET_DIR", "CARGO_MAKEFLAGS", "MAKEFLAGS",
    ] {
        cargo.env_remove(var);
    }
    let status = cargo.status().expect("could not run cargo to build the web UI");
    assert!(
        status.success(),
        "building the web UI failed. If the wasm target is missing, run: rustup target add wasm32-unknown-unknown"
    );

    let wasm = target_dir.join("wasm32-unknown-unknown").join(profile).join("hearth_web.wasm");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("ui");
    let mut bindgen = wasm_bindgen_cli_support::Bindgen::new();
    bindgen.input_path(&wasm).web(true).unwrap().typescript(false).out_name("hearth-web");
    bindgen.generate(&out).expect("wasm-bindgen could not process the web UI");
}
