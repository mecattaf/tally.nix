use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const BRIEF_DIRECTORY: &str = "briefs";
pub const MAX_BRIEF_BYTES: u64 = 16 * 1024 * 1024;

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq)]
pub struct PreparedBrief {
    document: Value,
    canonical: Vec<u8>,
    hash: String,
}

impl PreparedBrief {
    pub fn from_value(document: Value) -> Result<Self, BriefError> {
        let canonical = serde_json::to_vec(&document)
            .map_err(|error| BriefError::Invalid(format!("brief is not valid JSON: {error}")))?;
        if canonical.len() as u64 > MAX_BRIEF_BYTES {
            return Err(BriefError::Invalid(format!(
                "brief exceeds the {MAX_BRIEF_BYTES}-byte canonical limit"
            )));
        }
        let hash = format!("sha256:{:x}", Sha256::digest(&canonical));
        Ok(Self {
            document,
            canonical,
            hash,
        })
    }

    pub fn from_path(path: &Path) -> Result<Self, BriefError> {
        if !path.is_absolute() {
            return Err(BriefError::Invalid("briefPath must be absolute".to_owned()));
        }
        let bytes = read_regular_file_bounded(path)?;
        let document = serde_json::from_slice(&bytes).map_err(|error| {
            BriefError::Invalid(format!(
                "briefPath {} does not contain valid JSON: {error}",
                path.display()
            ))
        })?;
        Self::from_value(document)
    }

    pub fn hash(&self) -> &str {
        &self.hash
    }

    pub fn document(&self) -> &Value {
        &self.document
    }

    pub fn canonical(&self) -> &[u8] {
        &self.canonical
    }
}

#[derive(Debug, Error)]
pub enum BriefError {
    #[error("invalid structured brief: {0}")]
    Invalid(String),
    #[error("brief I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

fn io_error(path: &Path, source: std::io::Error) -> BriefError {
    BriefError::Io {
        path: path.to_owned(),
        source,
    }
}

pub fn prepare(
    inline: Option<Value>,
    source_path: Option<PathBuf>,
) -> Result<Option<PreparedBrief>, BriefError> {
    match (inline, source_path) {
        (None, None) => Ok(None),
        (Some(document), None) => PreparedBrief::from_value(document).map(Some),
        (None, Some(path)) => PreparedBrief::from_path(&path).map(Some),
        (Some(_), Some(_)) => Err(BriefError::Invalid(
            "enqueue accepts brief XOR briefPath, not both".to_owned(),
        )),
    }
}

pub fn content_path(root: &Path, hash: &str) -> Result<PathBuf, BriefError> {
    let digest = hash.strip_prefix("sha256:").ok_or_else(|| {
        BriefError::Invalid("briefHash must use the sha256:<hex> form".to_owned())
    })?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(BriefError::Invalid(
            "briefHash must be lowercase sha256 hex".to_owned(),
        ));
    }
    Ok(root.join(BRIEF_DIRECTORY).join(format!("{digest}.json")))
}

pub fn store(root: &Path, brief: &PreparedBrief) -> Result<PathBuf, BriefError> {
    let directory = root.join(BRIEF_DIRECTORY);
    ensure_private_directory(root, &directory)?;
    let final_path = content_path(root, brief.hash())?;
    if final_path.exists() {
        let existing = read_verified(&final_path, brief.hash())?;
        if existing.canonical != brief.canonical {
            return Err(BriefError::Invalid(format!(
                "content-addressed brief {} disagrees with its existing bytes",
                final_path.display()
            )));
        }
        return Ok(final_path);
    }

    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = directory.join(format!(".{}.{}.tmp", std::process::id(), sequence));
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)
            .map_err(|source| io_error(&temporary, source))?;
        file.write_all(brief.canonical())
            .and_then(|()| file.sync_all())
            .map_err(|source| io_error(&temporary, source))?;
        match std::fs::hard_link(&temporary, &final_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => return Err(io_error(&final_path, source)),
        }
        std::fs::remove_file(&temporary).map_err(|source| io_error(&temporary, source))?;
        File::open(&directory)
            .and_then(|file| file.sync_all())
            .map_err(|source| io_error(&directory, source))?;
        let existing = read_verified(&final_path, brief.hash())?;
        if existing.canonical != brief.canonical {
            return Err(BriefError::Invalid(format!(
                "content-addressed brief {} disagrees with the admitted document",
                final_path.display()
            )));
        }
        Ok(final_path.clone())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    write_result
}

pub fn read_verified(path: &Path, expected_hash: &str) -> Result<PreparedBrief, BriefError> {
    let bytes = read_regular_file_bounded(path)?;
    let document = serde_json::from_slice(&bytes).map_err(|error| {
        BriefError::Invalid(format!(
            "durable brief {} does not contain valid JSON: {error}",
            path.display()
        ))
    })?;
    let prepared = PreparedBrief::from_value(document)?;
    if prepared.hash() != expected_hash {
        return Err(BriefError::Invalid(format!(
            "durable brief {} hashes to {}, expected {expected_hash}",
            path.display(),
            prepared.hash()
        )));
    }
    if bytes != prepared.canonical {
        return Err(BriefError::Invalid(format!(
            "durable brief {} is not in canonical compact form",
            path.display()
        )));
    }
    Ok(prepared)
}

fn ensure_private_directory(root: &Path, directory: &Path) -> Result<(), BriefError> {
    let root_metadata = std::fs::symlink_metadata(root).map_err(|source| io_error(root, source))?;
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        return Err(BriefError::Invalid(format!(
            "brief storage root {} is not a real directory",
            root.display()
        )));
    }
    match std::fs::create_dir(directory) {
        Ok(()) => {
            File::open(root)
                .and_then(|file| file.sync_all())
                .map_err(|source| io_error(root, source))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(source) => return Err(io_error(directory, source)),
    }
    let metadata =
        std::fs::symlink_metadata(directory).map_err(|source| io_error(directory, source))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(BriefError::Invalid(format!(
            "brief storage {} is not a real directory",
            directory.display()
        )));
    }
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
        .map_err(|source| io_error(directory, source))
}

fn read_regular_file_bounded(path: &Path) -> Result<Vec<u8>, BriefError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|source| io_error(path, source))?;
    let metadata = file.metadata().map_err(|source| io_error(path, source))?;
    if !metadata.file_type().is_file() {
        return Err(BriefError::Invalid(format!(
            "brief {} is not a regular file",
            path.display()
        )));
    }
    if metadata.len() > MAX_BRIEF_BYTES {
        return Err(BriefError::Invalid(format!(
            "brief {} exceeds the {MAX_BRIEF_BYTES}-byte input limit",
            path.display()
        )));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_BRIEF_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    if bytes.len() as u64 > MAX_BRIEF_BYTES {
        return Err(BriefError::Invalid(format!(
            "brief {} exceeds the {MAX_BRIEF_BYTES}-byte input limit",
            path.display()
        )));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting_is_ignored_but_member_order_is_content() {
        let temp = tempfile::tempdir().unwrap();
        let formatted = temp.path().join("formatted.json");
        std::fs::write(
            &formatted,
            b"{\n  \"mission\": \"ship\", \"acceptance\": [1]\n}\n",
        )
        .unwrap();
        let from_path = PreparedBrief::from_path(&formatted).unwrap();
        let inline = PreparedBrief::from_value(serde_json::json!({
            "mission": "ship",
            "acceptance": [1]
        }))
        .unwrap();
        assert_eq!(from_path.hash(), inline.hash());

        let reordered = PreparedBrief::from_value(
            serde_json::from_str(r#"{"acceptance":[1],"mission":"ship"}"#).unwrap(),
        )
        .unwrap();
        assert_ne!(from_path.hash(), reordered.hash());
    }

    #[test]
    fn durable_store_is_private_canonical_and_content_addressed() {
        let temp = tempfile::tempdir().unwrap();
        let brief = PreparedBrief::from_value(serde_json::json!({"mission": "ship"})).unwrap();
        let path = store(temp.path(), &brief).unwrap();
        assert_eq!(read_verified(&path, brief.hash()).unwrap(), brief);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    fn source_contract_rejects_ambiguous_unsafe_and_oversized_inputs() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("brief.json");
        std::fs::write(&source, b"{}").unwrap();
        assert!(prepare(Some(serde_json::json!({})), Some(source.clone())).is_err());
        assert!(PreparedBrief::from_path(Path::new("relative.json")).is_err());
        assert!(PreparedBrief::from_path(temp.path()).is_err());

        let link = temp.path().join("link.json");
        std::os::unix::fs::symlink(&source, &link).unwrap();
        assert!(PreparedBrief::from_path(&link).is_err());

        let oversized = temp.path().join("oversized.json");
        File::create(&oversized)
            .unwrap()
            .set_len(MAX_BRIEF_BYTES + 1)
            .unwrap();
        assert!(PreparedBrief::from_path(&oversized).is_err());
        assert!(content_path(temp.path(), "sha256:not-hex").is_err());
    }

    #[test]
    fn durable_reader_rejects_noncanonical_or_hash_mismatched_content() {
        let temp = tempfile::tempdir().unwrap();
        let brief = PreparedBrief::from_value(serde_json::json!({
            "mission": "ship",
            "acceptance": [1]
        }))
        .unwrap();
        let path = store(temp.path(), &brief).unwrap();
        std::fs::write(
            &path,
            b"{\n  \"mission\": \"ship\",\n  \"acceptance\": [1]\n}\n",
        )
        .unwrap();
        assert!(read_verified(&path, brief.hash()).is_err());

        std::fs::write(&path, brief.canonical()).unwrap();
        let other_hash = format!("sha256:{}", "f".repeat(64));
        assert!(read_verified(&path, &other_hash).is_err());
    }
}
