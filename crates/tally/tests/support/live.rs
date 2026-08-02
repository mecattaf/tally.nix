// A test-harness skip notice; the cargo test runner owns this stream.
#![allow(clippy::disallowed_macros)]

pub const REMOTE_HOST_ENV: &str = "TALLY_TEST_REMOTE_HOST";

pub fn require_remote_host(test_name: &str) -> Option<String> {
    let host = std::env::var(REMOTE_HOST_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty());
    if host.is_none() {
        eprintln!(
            "SKIP {test_name}: set {REMOTE_HOST_ENV} and run this ignored test on that NixOS host"
        );
    }
    host
}
