//! The test-isolation guard (`EPSILON-EXTENSION.md` ext2, the F25 class).
//!
//! A `tally` spawned from a test inherits this process's environment, and this
//! process's environment is the operator's. `HOME` and the XDG roots name the
//! real config, state and data stores; `XDG_RUNTIME_DIR`/`TALLY_SOCKET` name
//! the real user daemon's socket; `TALLY_JOB_ID`/`TALLY_JOB_TOKEN` name a live
//! job capability when the suite is itself running inside a tally job unit.
//! Witnessed live on 2026-08-18: a chapter-gate `cargo test` dispatched job
//! units against the operator's own daemon and registry
//! (`specs/eta/evidence/run-log.md`).
//!
//! An [`IsolatedHost`] is one test's private stand-in for that host — its own
//! home, its own config/state/data/cache roots, its own runtime directory and
//! socket path. [`Isolated::isolated`] binds a subprocess to one, and
//! `tests/host_isolation.rs` proves both halves: that an unbound spawn really
//! does write into the home it inherited, and that a bound one cannot.
//!
//! The binding is applied first in a builder chain, so a test that wants to
//! aim one variable somewhere of its own — `direct_file_defaults.rs` proving
//! the data-dir precedence, say — keeps overriding it afterwards. What it
//! cannot do any more is silently inherit the operator's.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// The variables through which a spawned `tally` resolves a host location, and
/// the private directory each is bound to.
///
/// Read from the product's own resolvers: `tally-client`'s
/// `default_config_path` (`XDG_CONFIG_HOME`, else `HOME`), and `cli/exit.rs`'s
/// `default_socket_path` (`TALLY_SOCKET`, else `XDG_RUNTIME_DIR`),
/// `default_state_dir` (`XDG_STATE_HOME`, else `HOME`) and `default_data_dir`
/// (`TALLY_DATA_DIR`, else `XDG_DATA_HOME`, else `HOME`). Every fallback ends
/// at `HOME`, so `HOME` alone is not enough: a set `XDG_STATE_HOME` reaches
/// past a rebound home straight back to the operator's state.
///
/// `XDG_CACHE_HOME` is the one entry tally itself never reads: it is here for
/// what tally *launches*, which is the same reason `campaign.rs` insists an
/// adapter's build argv redirect it.
pub const BOUND_VARIABLES: [(&str, &str); 8] = [
    ("HOME", "home"),
    ("XDG_CONFIG_HOME", "home/.config"),
    ("XDG_STATE_HOME", "home/.local/state"),
    ("XDG_DATA_HOME", "home/.local/share"),
    ("XDG_CACHE_HOME", "home/.cache"),
    ("XDG_RUNTIME_DIR", "run"),
    ("TALLY_SOCKET", "run/tally.sock"),
    ("TALLY_DATA_DIR", "home/.local/share/tally"),
];

/// The ambient identity and service-manager attachments a suite running inside
/// a tally job unit inherits.
///
/// These name a *live* capability or a *live* peer on the spawning host rather
/// than a location, so there is no private value to bind them to: the isolated
/// spawn is one nobody minted a token for and nobody is supervising.
/// `NOTIFY_SOCKET` and the watchdog pair are systemd's, and a daemon spawned
/// under them reports readiness and liveness to the operator's own service
/// manager. Individual tests already scrubbed some of these one variable at a
/// time; the guard makes the whole set unconditional.
pub const SCRUBBED_VARIABLES: [&str; 10] = [
    "TALLY_JOB_ID",
    "TALLY_JOB_TOKEN",
    "TALLY_TASK_UUID",
    "TALLY_POOL",
    "TALLY_BRIEF",
    "TALLY_BRIEF_HASH",
    "TALLY_RPC_TIMEOUT_SEC",
    "NOTIFY_SOCKET",
    "WATCHDOG_PID",
    "WATCHDOG_USEC",
];

/// One test's private host.
///
/// Owned by the test, so the whole tree goes away with it; a test that spawns
/// a daemon and then talks to it keeps one host for both, which is what makes
/// the socket and state paths agree without either naming the other.
pub struct IsolatedHost {
    root: PathBuf,
    /// `Some` when this host owns its own temporary tree, `None` when the tree
    /// lies inside a directory the caller already owns.
    owned: Option<tempfile::TempDir>,
}

impl IsolatedHost {
    /// Create the private host and its directory tree in a fresh temporary
    /// directory this host owns.
    ///
    /// The tree is created eagerly: `XDG_RUNTIME_DIR` naming a directory that
    /// does not exist is not isolation, it is a different failure, and the one
    /// it produces reads as a product bug.
    pub fn new() -> Self {
        let owned = tempfile::tempdir().unwrap();
        Self::populate(Self {
            root: owned.path().to_owned(),
            owned: Some(owned),
        })
    }

    /// A private host rooted inside a directory the caller already owns.
    ///
    /// The tree outlives this value, which is what a helper that *builds* a
    /// command rather than running one needs: the child it is built for is
    /// spawned after the helper's frame is gone, and the host's lifetime
    /// becomes the caller's temporary root instead of the helper's stack.
    pub fn under(root: impl Into<PathBuf>) -> Self {
        Self::populate(Self {
            root: root.into(),
            owned: None,
        })
    }

    fn populate(host: Self) -> Self {
        for (_, relative) in BOUND_VARIABLES {
            let path = host.path(relative);
            // `TALLY_SOCKET` names the socket itself; its parent is the
            // directory the daemon binds into.
            let directory = if relative.ends_with(".sock") {
                path.parent().unwrap().to_owned()
            } else {
                path
            };
            std::fs::create_dir_all(&directory).unwrap();
        }
        host
    }

    /// The root every bound path lies under.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    /// The private value one bound variable takes, or `None` if the guard does
    /// not bind that variable.
    pub fn binding(&self, variable: &str) -> Option<PathBuf> {
        BOUND_VARIABLES
            .iter()
            .find(|(name, _)| *name == variable)
            .map(|(_, relative)| self.path(relative))
    }

    /// Every `(variable, private path)` pair this host binds.
    pub fn bindings(&self) -> Vec<(&'static str, PathBuf)> {
        BOUND_VARIABLES
            .iter()
            .map(|(name, relative)| (*name, self.path(relative)))
            .collect()
    }

    pub fn home(&self) -> PathBuf {
        self.path("home")
    }

    pub fn config_home(&self) -> PathBuf {
        self.path("home/.config")
    }

    pub fn state_home(&self) -> PathBuf {
        self.path("home/.local/state")
    }

    pub fn data_home(&self) -> PathBuf {
        self.path("home/.local/share")
    }

    pub fn runtime_dir(&self) -> PathBuf {
        self.path("run")
    }

    /// This host's private daemon socket.
    pub fn socket(&self) -> PathBuf {
        self.path("run/tally.sock")
    }
}

impl Default for IsolatedHost {
    fn default() -> Self {
        Self::new()
    }
}

/// Bind a subprocess builder to a private host.
///
/// Implemented for both command types the suite uses so one guard covers the
/// blocking and the async spawn sites alike.
pub trait Isolated {
    fn isolated(&mut self, host: &IsolatedHost) -> &mut Self;
}

macro_rules! isolated_command {
    ($command:ty) => {
        impl Isolated for $command {
            fn isolated(&mut self, host: &IsolatedHost) -> &mut Self {
                for (variable, value) in host.bindings() {
                    self.env(variable, value);
                }
                for variable in SCRUBBED_VARIABLES {
                    self.env_remove(variable);
                }
                self
            }
        }
    };
}

isolated_command!(std::process::Command);
isolated_command!(tokio::process::Command);
