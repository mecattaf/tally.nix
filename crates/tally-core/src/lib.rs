//! Core types and validation shared by the tally daemon and CLI.

pub mod adapters;
pub mod authorship;
pub mod brief;
pub mod completion;
pub mod config;
pub mod daemon;
pub mod evidence;
pub mod exec_attestation;
pub mod executor;
pub mod flow_lineage;
pub mod flow_membership;
pub mod git_ai;
pub mod history;
pub mod journal;
pub mod lease;
pub mod nix_store;
pub mod occupancy;
pub mod pagination;
pub mod poolset;
pub mod producer_query;
pub mod producers;
pub mod provenance;
pub mod query;
pub mod query_v2;
pub mod recovery;
pub mod retention;
pub mod storage;
pub mod taskdb;
pub mod trace;
pub mod unit_exit_migration;
pub mod usage;
pub mod usage_rollup;
pub mod watch;
pub mod wire;
pub mod witness;

pub use config::{Config, ConfigError, Enforce, Priority};

#[cfg(test)]
pub(crate) mod test_support {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    const SHELL_COMMAND_PROVIDER: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../test/fixtures/shell-command-provider"
    );

    fn shell_program_source(path: &Path) -> PathBuf {
        let mut source = OsString::from(path.as_os_str());
        source.push(".tally-test-script");
        PathBuf::from(source)
    }

    pub(crate) fn install_shell_program(path: &Path, body: impl AsRef<[u8]>) {
        std::fs::write(shell_program_source(path), body).unwrap();
        std::os::unix::fs::symlink(SHELL_COMMAND_PROVIDER, path).unwrap();
    }

    pub(crate) fn rewrite_shell_program(path: &Path, body: impl AsRef<[u8]>) {
        std::fs::write(shell_program_source(path), body).unwrap();
    }
}
