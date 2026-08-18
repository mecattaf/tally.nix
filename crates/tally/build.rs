use std::env;

/// The build's source revision is a property of the build, not of the source
/// bytes, so it arrives through the environment rather than through a
/// committed file. `nix build` passes `TALLY_BUILD_REV` (the flake's `self.rev`
/// on a clean checkout, `self.dirtyRev` when the ref carries uncommitted
/// bytes); a plain `cargo build` passes nothing and gets the literal `dev`, so
/// a build outside nix stays reproducible instead of stamping whatever the
/// ambient git state happened to be.
// Cargo's build-script protocol is `println!` lines on stdout and nothing
// else, and cargo is the only reader there is — the hung-up-reader mapping
// `cli::out` exists for has no meaning here.
#[allow(clippy::disallowed_macros)]
fn main() {
    println!("cargo::rerun-if-env-changed=TALLY_BUILD_REV");
    let revision = env::var("TALLY_BUILD_REV")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| String::from("dev"));
    println!("cargo::rustc-env=TALLY_BUILD_REV={revision}");
}
