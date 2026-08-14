//! Core types and validation shared by the tally daemon and CLI.
//!
//! Durability has one declared split: original inputs, observations, receipts,
//! and operator intent are canonical; task state, indexes, and query objects
//! are derived by replaying or rebuilding from those inputs. See
//! [`durability`] for the typed catalog and the invariant that keeps derived
//! state from becoming a second authority.

pub mod adapters;
pub mod assisted_by;
pub mod brief;
pub mod campaign_contract;
pub mod campaign_folds;
pub mod campaign_poll;
pub mod campaign_registry;
pub mod completion;
pub mod config;
pub mod daemon;
pub mod durability;
pub mod durable_view;
pub mod evidence;
pub mod exec_attestation;
pub mod executor;
pub mod flow_lineage;
pub mod flow_membership;
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
pub mod reader_state;
pub mod recovery;
pub mod retention;
pub mod storage;
pub mod taskdb;
pub mod trace;
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

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::unix::fs::PermissionsExt as _;

        /// Issue #396: every caller of this installer is immune to `ETXTBSY`
        /// for one reason only — the file the kernel is asked to `execve` is a
        /// checked-in fixture this process never opens. That is a property of
        /// the installer, so it is pinned here rather than once per caller.
        ///
        /// It is deliberately not "the installed program runs". A program
        /// written and `chmod +x`'d a microsecond earlier also runs, whenever
        /// no fork happens to be holding it — which is precisely the race that
        /// red-gated an innocent sha and never reproduced on a quiet host.
        #[test]
        fn an_installed_program_is_a_symlink_to_the_checked_in_provider_not_a_written_file() {
            let temporary = tempfile::tempdir().unwrap();
            let program = temporary.path().join("probe");
            install_shell_program(&program, "#!/bin/sh\nexit 0\n");

            let installed = std::fs::symlink_metadata(&program).unwrap();
            assert!(
                installed.file_type().is_symlink(),
                "the exec target must be a symlink to the checked-in provider, not a \
                 file this process wrote"
            );
            let target = std::fs::read_link(&program).unwrap();
            assert!(
                target.ends_with("test/fixtures/shell-command-provider"),
                "unexpected provider target {}",
                target.display()
            );
            assert!(
                !target.starts_with(temporary.path()),
                "the exec target resolves inside the directory this test writes into, \
                 so it is a file this process can hold open for writing: {}",
                target.display()
            );
            assert!(target.exists(), "{} is not checked in", target.display());

            let sidecar = shell_program_source(&program);
            assert_eq!(
                std::fs::metadata(&sidecar).unwrap().permissions().mode() & 0o111,
                0,
                "the file the installer writes must never be executable"
            );
        }
    }
}
