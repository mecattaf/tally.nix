use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::error::{DriverError, Result};
use crate::git::git;
use crate::json::{self, Json};
use crate::path::resolve;
use crate::sha256;

const CHANGE_SET_SNAPSHOT_NAME: &str = "tally-changeset-snapshot.json";
const WORKTREE_PREPARATION_LOCK_NAME: &str = "tally-worktree-preparation.lock";

#[cfg(target_os = "linux")]
const O_CLOEXEC: i32 = 0o2000000;
#[cfg(target_os = "linux")]
const O_NOFOLLOW: i32 = 0o400000;
const LOCK_EX: i32 = 2;

unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

pub(crate) type Identity = BTreeMap<String, String>;

#[derive(Clone, Debug)]
pub(crate) struct Resume {
    pub(crate) identity: Identity,
    pub(crate) complete: bool,
    pub(crate) head: String,
}

struct PreparationLock(File);

impl PreparationLock {
    fn acquire(checkout: &Path) -> Result<Self> {
        let path = preparation_lock_path(checkout)?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(O_CLOEXEC | O_NOFOLLOW)
            .open(&path)
            .map_err(|error| {
                DriverError::new(format!(
                    "cannot open linked-worktree preparation lock {}: {error}",
                    path.display()
                ))
            })?;
        // SAFETY: `file` owns a live descriptor for the duration of this guard.
        if unsafe { flock(file.as_raw_fd(), LOCK_EX) } != 0 {
            return Err(DriverError::new(format!(
                "cannot acquire linked-worktree preparation lock {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            )));
        }
        Ok(Self(file))
    }
}

impl Drop for PreparationLock {
    fn drop(&mut self) {
        // Closing the descriptor releases the advisory lock. Touch the field
        // so the ownership-bearing guard is explicit to readers and lints.
        let _ = self.0.as_raw_fd();
    }
}

fn preparation_lock_path(checkout: &Path) -> Result<PathBuf> {
    let common = git(
        checkout,
        ["rev-parse", "--path-format=absolute", "--git-common-dir"],
        true,
    )?
    .stdout_trimmed();
    Ok(PathBuf::from(common).join(WORKTREE_PREPARATION_LOCK_NAME))
}

fn validate_identity(identity: &Identity) -> Result<()> {
    for (key, value) in identity {
        let key_safe = !key.is_empty()
            && key.as_bytes()[0].is_ascii_lowercase()
            && key
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
        if !key_safe {
            return Err(DriverError::new(format!(
                "lane identity key {key:?} is not a safe git configuration key"
            )));
        }
        if value.is_empty()
            || value.len() > 512
            || !value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
        {
            return Err(DriverError::new(format!(
                "lane identity value for {key:?} is not storable in git configuration"
            )));
        }
    }
    Ok(())
}

fn enable_worktree_config(checkout: &Path) -> Result<()> {
    let current = git(
        checkout,
        ["config", "--get", "extensions.worktreeConfig"],
        false,
    )?;
    if current.stdout_trimmed().eq_ignore_ascii_case("true") {
        return Ok(());
    }
    git(
        checkout,
        ["config", "extensions.worktreeConfig", "true"],
        true,
    )?;
    Ok(())
}

pub(crate) fn parse_worktrees(checkout: &Path) -> Result<Vec<BTreeMap<String, String>>> {
    let listed = git(checkout, ["worktree", "list", "--porcelain"], true)?.stdout_text();
    let mut records = Vec::new();
    let mut current = BTreeMap::new();
    for line in listed.lines() {
        if line.is_empty() {
            if !current.is_empty() {
                records.push(std::mem::take(&mut current));
            }
            continue;
        }
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        current.insert(key.to_owned(), value.to_owned());
    }
    if !current.is_empty() {
        records.push(current);
    }
    Ok(records)
}

pub(crate) fn branch_exists(checkout: &Path, branch: &str) -> Result<bool> {
    Ok(git(
        checkout,
        [
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
        false,
    )?
    .success())
}

fn registered(checkout: &Path, worktree: &Path) -> Result<Option<BTreeMap<String, String>>> {
    let target = resolve(worktree)?;
    for record in parse_worktrees(checkout)? {
        let Some(raw) = record.get("worktree") else {
            continue;
        };
        if resolve(Path::new(raw))? == target {
            return Ok(Some(record));
        }
    }
    Ok(None)
}

pub(crate) fn is_registered(checkout: &Path, worktree: &Path) -> Result<bool> {
    Ok(registered(checkout, worktree)?.is_some())
}

fn same_repository(checkout: &Path, worktree: &Path) -> Result<bool> {
    let checkout_common = git(checkout, ["rev-parse", "--git-common-dir"], true)?.stdout_trimmed();
    let worktree_common = git(worktree, ["rev-parse", "--git-common-dir"], true)?.stdout_trimmed();
    let checkout_path = Path::new(&checkout_common);
    let checkout_path = if checkout_path.is_absolute() {
        checkout_path.to_owned()
    } else {
        checkout.join(checkout_path)
    };
    let worktree_path = Path::new(&worktree_common);
    let worktree_path = if worktree_path.is_absolute() {
        worktree_path.to_owned()
    } else {
        worktree.join(worktree_path)
    };
    Ok(resolve(&checkout_path)? == resolve(&worktree_path)?)
}

fn current_branch(worktree: &Path) -> Result<String> {
    Ok(git(worktree, ["branch", "--show-current"], true)?.stdout_trimmed())
}

pub(crate) fn read_identity(worktree: &Path) -> Result<Identity> {
    let viewed = git(
        worktree,
        ["config", "--worktree", "-z", "--get-regexp", r"^tally\."],
        false,
    )?;
    if !viewed.success() {
        return Ok(Identity::new());
    }
    let mut identity = Identity::new();
    for entry in viewed.stdout.split(|byte| *byte == 0) {
        if entry.is_empty() {
            continue;
        }
        let Some(separator) = entry.iter().position(|byte| *byte == b'\n') else {
            continue;
        };
        let key = String::from_utf8_lossy(&entry[..separator]);
        let value = String::from_utf8_lossy(&entry[separator + 1..]);
        if let Some(key) = key.strip_prefix("tally.") {
            identity.insert(key.to_owned(), value.into_owned());
        }
    }
    Ok(identity)
}

fn worktree_config_path(worktree: &Path) -> Result<PathBuf> {
    let git_dir =
        PathBuf::from(git(worktree, ["rev-parse", "--absolute-git-dir"], true)?.stdout_trimmed());
    let common_dir = PathBuf::from(
        git(
            worktree,
            ["rev-parse", "--path-format=absolute", "--git-common-dir"],
            true,
        )?
        .stdout_trimmed(),
    );
    if resolve(&git_dir)? == resolve(&common_dir)? {
        return Err(DriverError::new(format!(
            "{} is the main worktree and cannot carry lane identity",
            worktree.display()
        )));
    }
    Ok(git_dir.join("config.worktree"))
}

pub(crate) fn write_identity(worktree: &Path, identity: &Identity) -> Result<()> {
    validate_identity(identity)?;
    let path = worktree_config_path(worktree)?;
    let temporary = path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("config.worktree"),
        std::process::id()
    ));
    let result = (|| {
        match fs::remove_file(&temporary) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if path.exists() {
            fs::copy(&path, &temporary)?;
            let temporary_text = temporary.to_string_lossy().into_owned();
            git(
                worktree,
                [
                    "config",
                    "--file",
                    &temporary_text,
                    "--remove-section",
                    "tally",
                ],
                false,
            )?;
        } else {
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o644)
                .open(&temporary)?;
        }
        let temporary_text = temporary.to_string_lossy().into_owned();
        for (key, value) in identity {
            git(
                worktree,
                [
                    "config",
                    "--file",
                    &temporary_text,
                    &format!("tally.{key}"),
                    value,
                ],
                true,
            )?;
        }
        fs::rename(&temporary, &path)?;
        Ok(())
    })();
    let _ = fs::remove_file(&temporary);
    result
}

pub(crate) fn resume(
    checkout: &Path,
    worktree: &Path,
    expected: &Identity,
    required: &[&str],
) -> Result<Option<Resume>> {
    validate_identity(expected)?;
    enable_worktree_config(checkout)?;
    let Some(_record) = registered(checkout, worktree)? else {
        if worktree.exists() || fs::symlink_metadata(worktree).is_ok() {
            return Err(DriverError::new(format!(
                "path {} exists but is not a worktree of {}",
                worktree.display(),
                checkout.display()
            )));
        }
        return Ok(None);
    };
    if !worktree.is_dir() {
        prune(checkout)?;
        return Ok(None);
    }
    if !same_repository(checkout, worktree)? {
        return Err(DriverError::new(format!(
            "worktree {} is not a worktree of the configured checkout",
            worktree.display()
        )));
    }
    let recorded = read_identity(worktree)?;
    let mismatched: Vec<_> = expected
        .iter()
        .filter_map(|(key, value)| {
            recorded
                .get(key)
                .filter(|recorded| *recorded != value)
                .map(|_| key.clone())
        })
        .collect();
    if !mismatched.is_empty() {
        return Err(DriverError::new(format!(
            "worktree {} carries a different lane identity: {}",
            worktree.display(),
            mismatched.join(", ")
        )));
    }
    if let Some(branch) = expected.get("branch") {
        let actual = current_branch(worktree)?;
        if actual.is_empty() {
            if branch_exists(checkout, branch)? {
                git(worktree, ["switch", branch], true)?;
            } else {
                git(worktree, ["switch", "-c", branch], true)?;
            }
        } else if actual != *branch {
            return Err(DriverError::new(format!(
                "worktree {} is on branch {actual:?}, expected {branch:?}",
                worktree.display()
            )));
        }
    }
    let mut required_keys: BTreeSet<&str> = expected.keys().map(String::as_str).collect();
    required_keys.extend(required.iter().copied());
    let complete = required_keys.iter().all(|key| recorded.contains_key(*key));
    let head = git(worktree, ["rev-parse", "--verify", "HEAD^{commit}"], true)?.stdout_trimmed();
    Ok(Some(Resume {
        identity: recorded,
        complete,
        head,
    }))
}

pub(crate) fn add(
    checkout: &Path,
    worktree: &Path,
    branch: &str,
    start_rev: &str,
) -> Result<String> {
    let branch_check = git(checkout, ["check-ref-format", "--branch", branch], false)?;
    if !branch_check.success() {
        return Err(DriverError::new(format!(
            "branch name {branch:?} is not a valid git branch"
        )));
    }
    let _lock = PreparationLock::acquire(checkout)?;
    enable_worktree_config(checkout)?;
    if let Some(parent) = worktree.parent() {
        fs::create_dir_all(parent)?;
    }
    let worktree_text = worktree.to_string_lossy().into_owned();
    if branch_exists(checkout, branch)? {
        git(checkout, ["worktree", "add", &worktree_text, branch], true)?;
    } else {
        git(
            checkout,
            ["worktree", "add", "-b", branch, &worktree_text, start_rev],
            true,
        )?;
    }
    Ok(git(worktree, ["rev-parse", "--verify", "HEAD^{commit}"], true)?.stdout_trimmed())
}

pub(crate) fn remove(checkout: &Path, worktree: &Path, branch: Option<&str>) -> Result<()> {
    let worktree_text = worktree.to_string_lossy().into_owned();
    let removed = git(
        checkout,
        ["worktree", "remove", "--force", &worktree_text],
        false,
    )?;
    if !removed.success() && worktree.exists() {
        return Err(DriverError::new(format!(
            "cannot remove worktree {}: {}",
            worktree.display(),
            removed.detail()
        )));
    }
    let Some(branch) = branch else {
        return Ok(());
    };
    let deleted = git(checkout, ["branch", "-D", branch], false)?;
    if !deleted.success() && branch_exists(checkout, branch)? {
        return Err(DriverError::new(format!(
            "cannot remove branch {branch:?}: {}",
            deleted.detail()
        )));
    }
    Ok(())
}

pub(crate) fn prune(checkout: &Path) -> Result<()> {
    git(checkout, ["worktree", "prune"], false)?;
    Ok(())
}

fn snapshot_path(worktree: &Path) -> Result<PathBuf> {
    let git_dir = git(worktree, ["rev-parse", "--absolute-git-dir"], true)?.stdout_trimmed();
    Ok(PathBuf::from(git_dir).join(CHANGE_SET_SNAPSHOT_NAME))
}

fn unreadable_digest(metadata: &fs::Metadata) -> String {
    let mode = metadata.permissions().mode();
    let file_type = mode & 0o170000;
    let modified_ns = metadata
        .mtime()
        .saturating_mul(1_000_000_000)
        .saturating_add(metadata.mtime_nsec());
    let identity = format!("{file_type}:{mode:o}:{}:{modified_ns}", metadata.len());
    format!("unreadable:{}", sha256::digest(identity.as_bytes()))
}

pub(crate) fn change_set_fingerprint(worktree: &Path) -> Result<BTreeMap<String, String>> {
    let listed = git(
        worktree,
        [
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
        true,
    )?;
    let mut fingerprint = BTreeMap::new();
    for raw in listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let relative = String::from_utf8_lossy(raw).into_owned();
        let target = worktree.join(&relative);
        let metadata = match fs::symlink_metadata(&target) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if metadata.file_type().is_symlink() {
            let digest = match fs::read_link(&target) {
                Ok(link) => {
                    let bytes = link.as_os_str().as_bytes();
                    format!("symlink:{}", sha256::digest(bytes))
                }
                Err(_) => unreadable_digest(&metadata),
            };
            fingerprint.insert(relative, digest);
            continue;
        }
        let digest = match File::open(&target).and_then(sha256::digest_reader) {
            Ok(digest) => digest,
            Err(_) => unreadable_digest(&metadata),
        };
        fingerprint.insert(relative, digest);
    }
    Ok(fingerprint)
}

pub(crate) fn snapshot_exists(worktree: &Path) -> Result<bool> {
    Ok(snapshot_path(worktree)?.is_file())
}

pub(crate) fn write_snapshot(
    worktree: &Path,
    fingerprint: &BTreeMap<String, String>,
) -> Result<()> {
    let path = snapshot_path(worktree)?;
    let temporary = path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(CHANGE_SET_SNAPSHOT_NAME),
        std::process::id()
    ));
    let value = Json::Object(
        fingerprint
            .iter()
            .map(|(path, digest)| (path.clone(), Json::String(digest.clone())))
            .collect(),
    );
    let result = (|| {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(value.stringify().as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, &path)?;
        Ok(())
    })();
    let _ = fs::remove_file(&temporary);
    result
}

#[allow(dead_code)]
pub(crate) fn read_snapshot(worktree: &Path) -> Result<Option<BTreeMap<String, String>>> {
    let path = snapshot_path(worktree)?;
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let parsed = json::parse(&text)?;
    let object = parsed
        .as_object()
        .ok_or_else(|| DriverError::new("change-set snapshot must contain an object"))?;
    let mut snapshot = BTreeMap::new();
    for (path, value) in object {
        let digest = value
            .as_str()
            .ok_or_else(|| DriverError::new("change-set snapshot values must be strings"))?;
        snapshot.insert(path.clone(), digest.to_owned());
    }
    Ok(Some(snapshot))
}

#[cfg(test)]
mod tests {
    use super::{
        add, change_set_fingerprint, read_identity, remove, validate_identity, write_identity,
        PreparationLock,
    };
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    use crate::git::git;

    static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);

    struct Repository {
        root: PathBuf,
        checkout: PathBuf,
    }

    impl Repository {
        fn new() -> Self {
            let sequence = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "spec-build-driver-worktrees-{}-{sequence}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&root);
            let checkout = root.join("checkout");
            fs::create_dir_all(&checkout).unwrap();
            git(
                &checkout,
                ["init", "--quiet", "--initial-branch=main"],
                true,
            )
            .unwrap();
            git(&checkout, ["config", "user.name", "Tally Test"], true).unwrap();
            git(
                &checkout,
                ["config", "user.email", "tally-test@invalid"],
                true,
            )
            .unwrap();
            fs::write(checkout.join("root.txt"), "base\n").unwrap();
            git(&checkout, ["add", "root.txt"], true).unwrap();
            git(&checkout, ["commit", "--quiet", "-m", "initial"], true).unwrap();
            Self { root, checkout }
        }

        fn lane(&self, name: &str) -> PathBuf {
            self.root.join("lanes").join(name)
        }
    }

    impl Drop for Repository {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn worktrees_identity_rejects_unstorable_fields() {
        let mut identity = BTreeMap::from([("campaign".to_owned(), "fixture".to_owned())]);
        assert!(validate_identity(&identity).is_ok());
        identity.insert("not-safe".to_owned(), "fixture".to_owned());
        assert!(validate_identity(&identity).is_err());
        identity.remove("not-safe");
        identity.insert("campaign".to_owned(), "line\nbreak".to_owned());
        assert!(validate_identity(&identity).is_err());
    }

    #[test]
    fn worktrees_identity_round_trips_and_preserves_foreign_config() {
        let repository = Repository::new();
        let lane = repository.lane("identity");
        add(&repository.checkout, &lane, "lane/identity", "HEAD").unwrap();
        git(
            &lane,
            ["config", "--worktree", "other.key", "keep me"],
            true,
        )
        .unwrap();
        let identity = BTreeMap::from([
            ("campaign".to_owned(), "fixture".to_owned()),
            ("taskid".to_owned(), "task-1".to_owned()),
        ]);
        write_identity(&lane, &identity).unwrap();
        assert_eq!(read_identity(&lane).unwrap(), identity);
        assert_eq!(
            git(&lane, ["config", "--worktree", "--get", "other.key"], true,)
                .unwrap()
                .stdout_trimmed(),
            "keep me"
        );
        remove(&repository.checkout, &lane, Some("lane/identity")).unwrap();
    }

    #[test]
    fn worktrees_fresh_lane_cut_waits_for_the_shared_metadata_lock() {
        let repository = Repository::new();
        let held = PreparationLock::acquire(&repository.checkout).unwrap();
        let checkout = repository.checkout.clone();
        let lane = repository.lane("serialized");
        let lane_for_thread = lane.clone();
        let (sent, received) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = add(&checkout, &lane_for_thread, "lane/serialized", "HEAD");
            sent.send(result).unwrap();
        });
        assert!(
            received.recv_timeout(Duration::from_millis(150)).is_err(),
            "fresh lane creation crossed a held linked-worktree lock"
        );
        drop(held);
        received
            .recv_timeout(Duration::from_secs(5))
            .expect("lane creation did not resume after lock release")
            .unwrap();
        worker.join().unwrap();
        assert!(lane.is_dir());
        remove(&repository.checkout, &lane, Some("lane/serialized")).unwrap();
    }

    #[test]
    fn worktrees_fingerprint_covers_tracked_and_untracked_content() {
        let repository = Repository::new();
        let lane = repository.lane("fingerprint");
        add(&repository.checkout, &lane, "lane/fingerprint", "HEAD").unwrap();
        fs::write(lane.join("untracked.txt"), "one\n").unwrap();
        let first = change_set_fingerprint(&lane).unwrap();
        assert!(first.contains_key("root.txt"));
        assert!(first.contains_key("untracked.txt"));
        fs::write(lane.join("untracked.txt"), "two\n").unwrap();
        let second = change_set_fingerprint(&lane).unwrap();
        assert_ne!(first["untracked.txt"], second["untracked.txt"]);
        remove(&repository.checkout, &lane, Some("lane/fingerprint")).unwrap();
    }
}
