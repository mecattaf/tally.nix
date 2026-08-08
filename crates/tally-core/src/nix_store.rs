use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::witness::Derivation;

pub const STORE_CHECK_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

pub trait StoreValidity {
    fn check_validity(&self, path: &Path) -> Result<(), String>;
}

pub trait GcRootBackend {
    /// Return the containing store output for an asset path, if the backend
    /// owns that path. The production implementation recognizes
    /// `/nix/store/<hash-name>` and preserves any subpath only in the caller's
    /// authority record.
    fn containing_store_output(&self, path: &Path) -> Option<PathBuf> {
        containing_nix_store_output(path)
    }

    fn add_root(&self, link: &Path, target: &Path) -> Result<(), String>;

    /// Remove an indirect root. Missing roots are already reconciled.
    fn remove_root(&self, link: &Path) -> Result<(), String> {
        remove_gc_root(link).map_err(|error| error.to_string())
    }

    fn collect_garbage(&self) -> Result<(), String>;
}

/// Identify the complete Nix store output containing `path`.
///
/// Store subfiles are deliberately not collapsed by campaign authority: this
/// helper is only for choosing the GC-root target. Requiring the canonical
/// 32-character Nix base32 hash keeps a coincidental `/nix/store/...` prefix
/// from acquiring store semantics.
pub fn containing_nix_store_output(path: &Path) -> Option<PathBuf> {
    let relative = path.strip_prefix("/nix/store").ok()?;
    let mut components = relative.components();
    let std::path::Component::Normal(name) = components.next()? else {
        return None;
    };
    if components.any(|component| !matches!(component, std::path::Component::Normal(_))) {
        return None;
    }
    let name = name.to_str()?;
    let bytes = name.as_bytes();
    if bytes.len() < 34
        || bytes[32] != b'-'
        || !bytes[..32].iter().all(|byte| {
            matches!(
                byte,
                b'0'..=b'9'
                    | b'a'
                    | b'b'
                    | b'c'
                    | b'd'
                    | b'f'
                    | b'g'
                    | b'h'
                    | b'i'
                    | b'j'
                    | b'k'
                    | b'l'
                    | b'm'
                    | b'n'
                    | b'p'
                    | b'q'
                    | b'r'
                    | b's'
                    | b'v'
                    | b'w'
                    | b'x'
                    | b'y'
                    | b'z'
            )
        })
    {
        return None;
    }
    Some(Path::new("/nix/store").join(name))
}

/// Unlink one indirect GC root without treating an already-missing link as an
/// error. Removing the link is the supported lifetime boundary for roots made
/// with `nix-store --add-root`.
pub fn remove_gc_root(link: &Path) -> Result<(), std::io::Error> {
    match std::fs::remove_file(link) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(source),
    }
}

pub trait DerivationAvailability: Send + Sync {
    fn outputs_available_or_substitutable(&self, drv: &Derivation) -> Result<bool, String>;
}

#[derive(Debug, Clone)]
pub struct NixStore {
    nix_store_program: PathBuf,
    nix_program: PathBuf,
}

impl Default for NixStore {
    fn default() -> Self {
        Self {
            nix_store_program: program_from_env("TALLY_NIX_STORE_PROGRAM", "nix-store"),
            nix_program: program_from_env("TALLY_NIX_PROGRAM", "nix"),
        }
    }
}

fn program_from_env(variable: &str, fallback: &str) -> PathBuf {
    std::env::var_os(variable)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(fallback))
}

impl NixStore {
    #[cfg(test)]
    pub fn with_programs(nix_store_program: PathBuf, nix_program: PathBuf) -> Self {
        Self {
            nix_store_program,
            nix_program,
        }
    }

    pub fn outputs_available_or_substitutable(
        &self,
        drv: &Derivation,
    ) -> Result<bool, NixStoreError> {
        if drv
            .outputs
            .iter()
            .all(|output| self.check_validity_result(Path::new(&output.path)).is_ok())
        {
            return Ok(true);
        }

        let installable = format!("{}^*", drv.drv_path);
        let dry_run = run_command(
            &self.nix_program,
            [
                OsStr::new("build"),
                OsStr::new("--dry-run"),
                OsStr::new("--json"),
                OsStr::new("--no-link"),
                OsStr::new(&installable),
            ],
            None,
        )?;
        require_success(&self.nix_program, &dry_run)?;
        let dry_run_stderr = String::from_utf8_lossy(&dry_run.stderr);
        if dry_run_stderr.contains("will be built") {
            return Ok(false);
        }

        let substitution = run_command(
            &self.nix_program,
            [
                OsStr::new("build"),
                OsStr::new("--json"),
                OsStr::new("--no-link"),
                OsStr::new("--max-jobs"),
                OsStr::new("0"),
                OsStr::new(&installable),
            ],
            None,
        )?;
        require_success(&self.nix_program, &substitution)?;

        Ok(drv
            .outputs
            .iter()
            .all(|output| self.check_validity_result(Path::new(&output.path)).is_ok()))
    }

    pub fn check_validity_result(&self, path: &Path) -> Result<(), NixStoreError> {
        let output = run_command(
            &self.nix_store_program,
            [OsStr::new("--check-validity"), path.as_os_str()],
            Some(STORE_CHECK_TIMEOUT),
        )?;
        require_success(&self.nix_store_program, &output)
    }

    fn add_root_result(&self, link: &Path, target: &Path) -> Result<(), NixStoreError> {
        let parent = link.parent().ok_or_else(|| NixStoreError::InvalidRoot {
            link: link.to_owned(),
            reason: "root link has no parent directory".to_owned(),
        })?;
        std::fs::create_dir_all(parent).map_err(|source| NixStoreError::RootIo {
            path: parent.to_owned(),
            source,
        })?;
        let output = run_command(
            &self.nix_store_program,
            [
                OsStr::new("--add-root"),
                link.as_os_str(),
                OsStr::new("--realise"),
                target.as_os_str(),
            ],
            None,
        )?;
        require_success(&self.nix_store_program, &output)
    }

    fn collect_garbage_result(&self) -> Result<(), NixStoreError> {
        let output = run_command(
            &self.nix_program,
            [OsStr::new("store"), OsStr::new("gc")],
            None,
        )?;
        require_success(&self.nix_program, &output)
    }

    fn remove_root_result(&self, link: &Path) -> Result<(), NixStoreError> {
        remove_gc_root(link).map_err(|source| NixStoreError::RootIo {
            path: link.to_owned(),
            source,
        })
    }
}

impl StoreValidity for NixStore {
    fn check_validity(&self, path: &Path) -> Result<(), String> {
        self.check_validity_result(path)
            .map_err(|error| error.to_string())
    }
}

impl DerivationAvailability for NixStore {
    fn outputs_available_or_substitutable(&self, drv: &Derivation) -> Result<bool, String> {
        NixStore::outputs_available_or_substitutable(self, drv).map_err(|error| error.to_string())
    }
}

impl GcRootBackend for NixStore {
    fn add_root(&self, link: &Path, target: &Path) -> Result<(), String> {
        self.add_root_result(link, target)
            .map_err(|error| error.to_string())
    }

    fn remove_root(&self, link: &Path) -> Result<(), String> {
        self.remove_root_result(link)
            .map_err(|error| error.to_string())
    }

    fn collect_garbage(&self) -> Result<(), String> {
        self.collect_garbage_result()
            .map_err(|error| error.to_string())
    }
}

#[derive(Debug, Error)]
pub enum NixStoreError {
    #[error("cannot spawn {program}: {source}")]
    Spawn {
        program: PathBuf,
        source: std::io::Error,
    },
    #[error("{program} timed out after {seconds}s")]
    Timeout { program: PathBuf, seconds: u64 },
    #[error("cannot wait for {program}: {source}")]
    Wait {
        program: PathBuf,
        source: std::io::Error,
    },
    #[error("{program} exited with {status}: {detail}")]
    Exit {
        program: PathBuf,
        status: ExitStatus,
        detail: String,
    },
    #[error("invalid GC root {link}: {reason}")]
    InvalidRoot { link: PathBuf, reason: String },
    #[error("cannot prepare GC root directory {path}: {source}")]
    RootIo {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[derive(Debug)]
struct CommandOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn run_command(
    program: &Path,
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
    timeout: Option<Duration>,
) -> Result<CommandOutput, NixStoreError> {
    let args = args
        .into_iter()
        .map(|argument| argument.as_ref().to_os_string())
        .collect::<Vec<OsString>>();
    let mut child = Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| NixStoreError::Spawn {
            program: program.to_owned(),
            source,
        })?;
    let mut stdout = child
        .stdout
        .take()
        .expect("piped command stdout is always present");
    let mut stderr = child
        .stderr
        .take()
        .expect("piped command stderr is always present");
    let stdout_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        bytes
    });

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if timeout.is_none_or(|timeout| started.elapsed() < timeout) => {
                thread::sleep(POLL_INTERVAL)
            }
            Ok(None) => {
                let timeout = timeout.expect("an elapsed command timeout is present");
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(NixStoreError::Timeout {
                    program: program.to_owned(),
                    seconds: timeout.as_secs(),
                });
            }
            Err(source) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(NixStoreError::Wait {
                    program: program.to_owned(),
                    source,
                });
            }
        }
    };
    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    Ok(CommandOutput {
        status,
        stdout,
        stderr,
    })
}

fn require_success(program: &Path, output: &CommandOutput) -> Result<(), NixStoreError> {
    if output.status.success() {
        return Ok(());
    }
    let detail_bytes = if output.stderr.is_empty() {
        output.stdout.as_slice()
    } else {
        output.stderr.as_slice()
    };
    let truncated = &detail_bytes[..detail_bytes.len().min(4096)];
    let detail = String::from_utf8_lossy(truncated);
    let detail = detail.trim();
    let detail = if detail_bytes.len() > truncated.len() {
        format!("{detail}…")
    } else if detail.is_empty() {
        "no diagnostic output".to_owned()
    } else {
        detail.to_owned()
    };
    Err(NixStoreError::Exit {
        program: program.to_owned(),
        status: output.status,
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell_program(path: &Path, body: &str) {
        crate::test_support::install_shell_program(path, body);
    }

    #[test]
    fn validity_is_fail_closed_for_nonzero_and_spawn_failure() {
        let temp = tempfile::tempdir().unwrap();
        let failing = temp.path().join("failing-nix-store");
        shell_program(&failing, "#!/bin/sh\nexit 7\n");
        let missing = temp.path().join("missing-nix");
        let store = NixStore::with_programs(failing, missing.clone());
        assert!(store
            .check_validity_result(Path::new(
                "/nix/store/00000000000000000000000000000000-output"
            ))
            .is_err());

        let store = NixStore::with_programs(missing.clone(), missing);
        assert!(matches!(
            store.check_validity_result(Path::new(
                "/nix/store/00000000000000000000000000000000-output"
            )),
            Err(NixStoreError::Spawn { .. })
        ));
    }

    #[test]
    fn containing_output_keeps_store_subfiles_out_of_the_root_target() {
        let output = Path::new("/nix/store/0123456789abcdfghijklmnpqrsvwxyz-tally-1.0");
        assert_eq!(
            containing_nix_store_output(&output.join("share/tally/spec-build.js")),
            Some(output.to_owned())
        );
        assert_eq!(containing_nix_store_output(output), Some(output.to_owned()));
        assert_eq!(
            containing_nix_store_output(Path::new("/nix/store/not-a-store-output/bin/tool")),
            None
        );
        assert_eq!(
            containing_nix_store_output(Path::new(
                "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-tally/../other"
            )),
            None
        );
    }

    #[test]
    fn removing_a_gc_root_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let link = temp.path().join("root");
        std::os::unix::fs::symlink("/nix/store/target", &link).unwrap();
        remove_gc_root(&link).unwrap();
        remove_gc_root(&link).unwrap();
        assert!(std::fs::symlink_metadata(&link).is_err());
    }
}
