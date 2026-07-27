use std::fmt;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::nix_store::{NixStore, StoreValidity};
use crate::witness::{Verdict, WitnessRecord};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceCheck {
    Artifact(PathBuf),
    Store(PathBuf),
    HashSha256 { expected: Option<String> },
    Exit(i32),
}

impl fmt::Display for EvidenceCheck {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Artifact(path) => write!(formatter, "artifact:{}", path.display()),
            Self::Store(path) => write!(formatter, "store:{}", path.display()),
            Self::HashSha256 { expected: None } => formatter.write_str("hash:sha256"),
            Self::HashSha256 {
                expected: Some(expected),
            } => write!(
                formatter,
                "hash:sha256:{}",
                expected.strip_prefix("sha256:").unwrap_or(expected)
            ),
            Self::Exit(code) => write!(formatter, "exit:{code}"),
        }
    }
}

impl FromStr for EvidenceCheck {
    type Err = EvidenceError;

    fn from_str(spec: &str) -> Result<Self, Self::Err> {
        let (kind, value) = spec.split_once(':').ok_or_else(|| {
            EvidenceError::InvalidSpec(format!("evidence spec must be <kind>:<value>: {spec:?}"))
        })?;
        match kind {
            "artifact" if value.is_empty() => Err(EvidenceError::InvalidSpec(
                "artifact evidence requires a path".to_owned(),
            )),
            "artifact" => {
                let path = PathBuf::from(value);
                if !path.is_absolute() {
                    return Err(EvidenceError::InvalidSpec(
                        "artifact evidence requires an absolute path".to_owned(),
                    ));
                }
                Ok(Self::Artifact(path))
            }
            "store" if crate::witness::is_nix_store_path(value) => {
                Ok(Self::Store(PathBuf::from(value)))
            }
            "store" => Err(EvidenceError::InvalidSpec(
                "store evidence requires an absolute canonical Nix store path".to_owned(),
            )),
            "hash" => {
                let (algorithm, expected) = value
                    .split_once(':')
                    .map_or((value, None), |(algorithm, expected)| {
                        (algorithm, Some(expected))
                    });
                if algorithm != "sha256" {
                    return Err(EvidenceError::UnsupportedHash(algorithm.to_owned()));
                }
                let expected = expected.map(normalize_sha256).transpose()?;
                Ok(Self::HashSha256 { expected })
            }
            "exit" => {
                let code = value.parse::<i32>().map_err(|_| {
                    EvidenceError::InvalidSpec(format!(
                        "exit evidence requires an integer code: {spec:?}"
                    ))
                })?;
                if !(0..=255).contains(&code) {
                    return Err(EvidenceError::InvalidSpec(format!(
                        "exit evidence code must be in 0..=255: {code}"
                    )));
                }
                Ok(Self::Exit(code))
            }
            _ => Err(EvidenceError::InvalidSpec(format!(
                "unknown evidence kind {kind:?}; expected artifact, store, hash, or exit"
            ))),
        }
    }
}

fn normalize_sha256(value: &str) -> Result<String, EvidenceError> {
    let hex = value.strip_prefix("sha256:").unwrap_or(value);
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(EvidenceError::InvalidSpec(
            "a fixed sha256 value must contain exactly 64 hexadecimal digits".to_owned(),
        ));
    }
    Ok(format!("sha256:{}", hex.to_ascii_lowercase()))
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EvidenceSpec {
    checks: Vec<EvidenceCheck>,
}

impl EvidenceSpec {
    pub fn new(mut checks: Vec<EvidenceCheck>) -> Result<Self, EvidenceError> {
        let mut hash_seen = false;
        let mut exit_seen = false;
        let mut store_paths = std::collections::BTreeSet::new();
        for check in &mut checks {
            match check {
                EvidenceCheck::Artifact(path) if path.as_os_str().is_empty() => {
                    return Err(EvidenceError::InvalidSpec(
                        "artifact evidence requires a path".to_owned(),
                    ));
                }
                EvidenceCheck::Artifact(path) if path.to_str().is_none() => {
                    return Err(EvidenceError::InvalidSpec(
                        "artifact evidence paths must be valid UTF-8".to_owned(),
                    ));
                }
                EvidenceCheck::Artifact(path) if !path.is_absolute() => {
                    return Err(EvidenceError::InvalidSpec(
                        "artifact evidence requires an absolute path".to_owned(),
                    ));
                }
                EvidenceCheck::Store(path)
                    if !path.to_str().is_some_and(crate::witness::is_nix_store_path) =>
                {
                    return Err(EvidenceError::InvalidSpec(
                        "store evidence requires an absolute canonical Nix store path".to_owned(),
                    ));
                }
                EvidenceCheck::Store(path) if !store_paths.insert(path.clone()) => {
                    return Err(EvidenceError::DuplicateStore(path.clone()));
                }
                EvidenceCheck::HashSha256 { .. } if hash_seen => {
                    return Err(EvidenceError::DuplicateCheck("hash"));
                }
                EvidenceCheck::HashSha256 { expected } => {
                    if let Some(value) = expected {
                        *value = normalize_sha256(value)?;
                    }
                    hash_seen = true;
                }
                EvidenceCheck::Exit(_) if exit_seen => {
                    return Err(EvidenceError::DuplicateCheck("exit"));
                }
                EvidenceCheck::Exit(code) if !(0..=255).contains(code) => {
                    return Err(EvidenceError::InvalidSpec(format!(
                        "exit evidence code must be in 0..=255: {code}"
                    )));
                }
                EvidenceCheck::Exit(_) => exit_seen = true,
                EvidenceCheck::Artifact(_) | EvidenceCheck::Store(_) => {}
            }
        }
        Ok(Self { checks })
    }

    pub fn parse<'a>(specs: impl IntoIterator<Item = &'a str>) -> Result<Self, EvidenceError> {
        Self::new(
            specs
                .into_iter()
                .map(str::parse)
                .collect::<Result<Vec<_>, _>>()?,
        )
    }

    pub fn checks(&self) -> &[EvidenceCheck] {
        &self.checks
    }

    pub fn render(&self) -> Vec<String> {
        self.checks.iter().map(ToString::to_string).collect()
    }

    fn artifact_paths(&self) -> impl Iterator<Item = &Path> {
        self.checks.iter().filter_map(|check| match check {
            EvidenceCheck::Artifact(path) => Some(path.as_path()),
            _ => None,
        })
    }

    fn store_paths(&self) -> impl Iterator<Item = &Path> {
        self.checks.iter().filter_map(|check| match check {
            EvidenceCheck::Store(path) => Some(path.as_path()),
            _ => None,
        })
    }

    pub fn declared_store_paths(&self) -> Vec<String> {
        let mut paths = self
            .store_paths()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        paths
    }

    fn expected_exit(&self) -> i32 {
        self.checks
            .iter()
            .find_map(|check| match check {
                EvidenceCheck::Exit(code) => Some(*code),
                _ => None,
            })
            .unwrap_or(0)
    }

    fn hash_check(&self) -> Option<&EvidenceCheck> {
        self.checks
            .iter()
            .find(|check| matches!(check, EvidenceCheck::HashSha256 { .. }))
    }
}

pub fn parse_evidence_specs(specs: &[String]) -> Result<EvidenceSpec, EvidenceError> {
    EvidenceSpec::parse(specs.iter().map(String::as_str))
}

#[derive(Debug, Error)]
pub enum EvidenceError {
    #[error("invalid evidence specification: {0}")]
    InvalidSpec(String),
    #[error("unsupported evidence hash algorithm {0:?}; only sha256 is supported")]
    UnsupportedHash(String),
    #[error("evidence specification contains more than one {0} check")]
    DuplicateCheck(&'static str),
    #[error("evidence specification contains duplicate store path {0}")]
    DuplicateStore(PathBuf),
    #[error("cannot read artifact {path}: {source}")]
    ArtifactIo {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("artifact {0} is not a regular file")]
    NotRegularFile(PathBuf),
    #[error("artifact {0} changed while it was being hashed")]
    ArtifactChanged(PathBuf),
}

fn artifact_io(path: &Path, source: std::io::Error) -> EvidenceError {
    EvidenceError::ArtifactIo {
        path: path.to_owned(),
        source,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    device: u64,
    inode: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl FileStamp {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.size(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

pub fn hash_artifact_file(path: &Path) -> Result<String, EvidenceError> {
    hash_artifact_file_with_hook(path, || {})
}

fn hash_artifact_file_with_hook(
    path: &Path,
    after_open: impl FnOnce(),
) -> Result<String, EvidenceError> {
    let path_before = std::fs::metadata(path).map_err(|source| artifact_io(path, source))?;
    if !path_before.file_type().is_file() {
        return Err(EvidenceError::NotRegularFile(path.to_owned()));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|source| artifact_io(path, source))?;
    let file_before = file
        .metadata()
        .map_err(|source| artifact_io(path, source))?;
    if !file_before.file_type().is_file() {
        return Err(EvidenceError::NotRegularFile(path.to_owned()));
    }
    if FileStamp::from_metadata(&path_before) != FileStamp::from_metadata(&file_before) {
        return Err(EvidenceError::ArtifactChanged(path.to_owned()));
    }

    after_open();

    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| artifact_io(path, source))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let file_after = file
        .metadata()
        .map_err(|source| artifact_io(path, source))?;
    let path_after =
        std::fs::metadata(path).map_err(|_| EvidenceError::ArtifactChanged(path.to_owned()))?;
    let stable = FileStamp::from_metadata(&file_before) == FileStamp::from_metadata(&file_after)
        && FileStamp::from_metadata(&file_after) == FileStamp::from_metadata(&path_after);
    if !stable {
        return Err(EvidenceError::ArtifactChanged(path.to_owned()));
    }
    Ok(format!("sha256:{:x}", digest.finalize()))
}

pub fn combine_artifact_hashes(hashes: &[String]) -> Option<String> {
    match hashes {
        [] => None,
        [hash] => Some(hash.clone()),
        hashes => {
            let digest = Sha256::digest(hashes.join("\n").as_bytes());
            Some(format!("sha256:{digest:x}"))
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunOutcome<'a> {
    pub exit_code: i32,
    pub wall_clock_seconds: f64,
    pub evidence: &'a EvidenceSpec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckOutcome {
    pub spec: String,
    pub passed: bool,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateResult {
    pub verdict: Verdict,
    pub passed: bool,
    pub artifact_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_paths: Option<Vec<String>>,
    pub checks: Vec<CheckOutcome>,
    pub clean_exit_no_artifact: bool,
}

pub fn run_evidence_gate(outcome: RunOutcome<'_>) -> GateResult {
    run_evidence_gate_with_store(outcome, &NixStore::default())
}

pub fn run_evidence_gate_with_store(
    outcome: RunOutcome<'_>,
    store: &impl StoreValidity,
) -> GateResult {
    let mut checks = Vec::new();

    let expected_exit = outcome.evidence.expected_exit();
    let exit_ok = outcome.exit_code == expected_exit;
    checks.push(CheckOutcome {
        spec: format!("exit:{expected_exit}"),
        passed: exit_ok,
        reason: if exit_ok {
            format!("exit code {} == {expected_exit}", outcome.exit_code)
        } else {
            format!(
                "exit code {} != expected {expected_exit}",
                outcome.exit_code
            )
        },
    });

    let span_ok = outcome.wall_clock_seconds.is_finite() && outcome.wall_clock_seconds >= 0.0;
    checks.push(CheckOutcome {
        spec: "witness-span".to_owned(),
        passed: span_ok,
        reason: if span_ok {
            format!("witness span {}s recorded", outcome.wall_clock_seconds)
        } else {
            "witness span is absent, negative, or non-finite".to_owned()
        },
    });

    let artifact_paths: Vec<&Path> = outcome.evidence.artifact_paths().collect();
    let mut artifacts_ok = true;
    let mut hashes = Vec::with_capacity(artifact_paths.len());
    for path in &artifact_paths {
        match hash_artifact_file(path) {
            Ok(hash) => {
                checks.push(CheckOutcome {
                    spec: format!("artifact:{}", path.display()),
                    passed: true,
                    reason: format!("artifact exists ({hash})"),
                });
                hashes.push(hash);
            }
            Err(error) => {
                artifacts_ok = false;
                checks.push(CheckOutcome {
                    spec: format!("artifact:{}", path.display()),
                    passed: false,
                    reason: error.to_string(),
                });
            }
        }
    }

    let mut artifact_hash = if artifacts_ok {
        combine_artifact_hashes(&hashes)
    } else {
        None
    };
    let store_paths = outcome.evidence.store_paths().collect::<Vec<_>>();
    let mut passing_store_paths = Vec::with_capacity(store_paths.len());
    let mut stores_ok = true;
    for path in &store_paths {
        match store.check_validity(path) {
            Ok(()) => {
                checks.push(CheckOutcome {
                    spec: format!("store:{}", path.display()),
                    passed: true,
                    reason: "store path is valid".to_owned(),
                });
                passing_store_paths.push(path.to_string_lossy().into_owned());
            }
            Err(reason) => {
                stores_ok = false;
                checks.push(CheckOutcome {
                    spec: format!("store:{}", path.display()),
                    passed: false,
                    reason,
                });
            }
        }
    }
    passing_store_paths.sort();
    passing_store_paths.dedup();
    let passing_store_paths = (!passing_store_paths.is_empty()).then_some(passing_store_paths);

    let mut artifact_required = !artifact_paths.is_empty() || !store_paths.is_empty();
    if let Some(EvidenceCheck::HashSha256 { expected }) = outcome.evidence.hash_check() {
        artifact_required = true;
        let hash_ok = expected.as_ref().map_or_else(
            || artifact_hash.is_some(),
            |value| artifact_hash.as_ref() == Some(value),
        );
        let spec = EvidenceCheck::HashSha256 {
            expected: expected.clone(),
        }
        .to_string();
        checks.push(CheckOutcome {
            spec,
            passed: hash_ok,
            reason: match (hash_ok, expected, artifact_hash.as_deref()) {
                (true, Some(expected), _) => format!("content hash matches {expected}"),
                (true, None, Some(actual)) => format!("content hash {actual} recorded"),
                (false, Some(expected), Some(actual)) => {
                    format!("content hash {actual} != declared {expected}")
                }
                (false, Some(expected), None) => {
                    format!("content hash <none> != declared {expected}")
                }
                (false, None, None) => "no artifact to hash".to_owned(),
                (false, None, Some(_)) | (true, None, None) => {
                    unreachable!("hash-check truth table is exhaustive")
                }
            },
        });
        if !hash_ok {
            artifacts_ok = false;
        }
    }

    let gate_passed = exit_ok && span_ok && (!artifact_required || (artifacts_ok && stores_ok));
    if gate_passed {
        return GateResult {
            verdict: Verdict::Pass,
            passed: true,
            artifact_hash,
            store_paths: passing_store_paths,
            checks,
            clean_exit_no_artifact: false,
        };
    }

    if exit_ok && span_ok && artifact_required && (!artifacts_ok || !stores_ok) {
        artifact_hash = None;
        return GateResult {
            verdict: Verdict::CleanExitNoArtifact,
            passed: false,
            artifact_hash,
            store_paths: passing_store_paths,
            checks,
            clean_exit_no_artifact: true,
        };
    }

    GateResult {
        verdict: Verdict::Failed,
        passed: false,
        artifact_hash,
        store_paths: passing_store_paths,
        checks,
        clean_exit_no_artifact: false,
    }
}

pub fn checked_paths(result: &GateResult) -> Vec<String> {
    result
        .checks
        .iter()
        .map(|check| check.spec.clone())
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DedupMissReason {
    NoKey,
    NoPriorPass,
    NoArtifactEvidence,
    ArtifactUnavailable(PathBuf),
    WitnessHashMismatch,
    DeclaredHashMismatch,
    StorePathInvalid(PathBuf),
    WitnessStorePathsMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DedupResult {
    pub hit: bool,
    pub dedup_key: Option<String>,
    pub artifact_hash: Option<String>,
    pub store_paths: Option<Vec<String>>,
    pub matched_witness_seq: Option<u64>,
    pub rehashed: bool,
    pub miss_reason: Option<DedupMissReason>,
}

fn dedup_miss(dedup_key: Option<&str>, reason: DedupMissReason, rehashed: bool) -> DedupResult {
    DedupResult {
        hit: false,
        dedup_key: dedup_key.map(str::to_owned),
        artifact_hash: None,
        store_paths: None,
        matched_witness_seq: None,
        rehashed,
        miss_reason: Some(reason),
    }
}

pub fn probe_dedup(
    dedup_key: Option<&str>,
    evidence: &EvidenceSpec,
    witness: &[WitnessRecord],
) -> DedupResult {
    probe_dedup_with_store(dedup_key, evidence, witness, &NixStore::default())
}

pub fn probe_dedup_with_store(
    dedup_key: Option<&str>,
    evidence: &EvidenceSpec,
    witness: &[WitnessRecord],
    store: &impl StoreValidity,
) -> DedupResult {
    let Some(dedup_key) = dedup_key.filter(|key| !key.trim().is_empty()) else {
        return dedup_miss(dedup_key, DedupMissReason::NoKey, false);
    };

    let Some(matched) = witness
        .iter()
        .filter(|record| {
            record.dedup_key.as_deref() == Some(dedup_key)
                && record.verdict == Verdict::Pass
                && (record.artifact_content_hash.is_some() || record.store_paths.is_some())
        })
        .max_by_key(|record| record.seq)
    else {
        return dedup_miss(Some(dedup_key), DedupMissReason::NoPriorPass, false);
    };

    let paths: Vec<&Path> = evidence.artifact_paths().collect();
    let store_paths = evidence.store_paths().collect::<Vec<_>>();
    if paths.is_empty() && store_paths.is_empty() {
        return dedup_miss(Some(dedup_key), DedupMissReason::NoArtifactEvidence, false);
    }

    probe_matching_record(Some(dedup_key), evidence, matched, store, false)
}

fn probe_matching_record(
    dedup_key: Option<&str>,
    evidence: &EvidenceSpec,
    matched: &WitnessRecord,
    store: &impl StoreValidity,
    allow_empty: bool,
) -> DedupResult {
    let paths: Vec<&Path> = evidence.artifact_paths().collect();
    let declared_store_paths = evidence.store_paths().collect::<Vec<_>>();
    if paths.is_empty() && declared_store_paths.is_empty() && allow_empty {
        return DedupResult {
            hit: true,
            dedup_key: dedup_key.map(str::to_owned),
            artifact_hash: matched.artifact_content_hash.clone(),
            store_paths: matched.store_paths.clone(),
            matched_witness_seq: Some(matched.seq),
            rehashed: false,
            miss_reason: None,
        };
    }

    let mut hashes = Vec::with_capacity(paths.len());
    for path in &paths {
        match hash_artifact_file(path) {
            Ok(hash) => hashes.push(hash),
            Err(_) => {
                return dedup_miss(
                    dedup_key,
                    DedupMissReason::ArtifactUnavailable((*path).to_owned()),
                    true,
                );
            }
        }
    }
    let current_hash = combine_artifact_hashes(&hashes);

    if evidence
        .hash_check()
        .and_then(|check| match check {
            EvidenceCheck::HashSha256 { expected } => expected.as_ref(),
            _ => None,
        })
        .is_some_and(|expected| Some(expected) != current_hash.as_ref())
    {
        return dedup_miss(dedup_key, DedupMissReason::DeclaredHashMismatch, true);
    }

    if !paths.is_empty() && matched.artifact_content_hash != current_hash {
        return dedup_miss(dedup_key, DedupMissReason::WitnessHashMismatch, true);
    }

    let mut canonical_store_paths = declared_store_paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    canonical_store_paths.sort();
    canonical_store_paths.dedup();
    for path in &declared_store_paths {
        if store.check_validity(path).is_err() {
            return dedup_miss(
                dedup_key,
                DedupMissReason::StorePathInvalid((*path).to_owned()),
                true,
            );
        }
    }
    let store_paths = (!canonical_store_paths.is_empty()).then_some(canonical_store_paths);
    if !declared_store_paths.is_empty() && matched.store_paths != store_paths {
        return dedup_miss(dedup_key, DedupMissReason::WitnessStorePathsMismatch, true);
    }

    DedupResult {
        hit: true,
        dedup_key: dedup_key.map(str::to_owned),
        artifact_hash: current_hash,
        store_paths,
        matched_witness_seq: Some(matched.seq),
        rehashed: true,
        miss_reason: None,
    }
}

pub fn probe_full_pass(evidence: &EvidenceSpec, matched: &WitnessRecord) -> DedupResult {
    probe_full_pass_with_store(evidence, matched, &NixStore::default())
}

pub fn probe_full_pass_with_store(
    evidence: &EvidenceSpec,
    matched: &WitnessRecord,
    store: &impl StoreValidity,
) -> DedupResult {
    probe_matching_record(matched.dedup_key.as_deref(), evidence, matched, store, true)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryTrigger {
    PoolReturn,
    ResourceReturn,
    BoundedRequeue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    pub auto_pool_return: bool,
    pub auto_resource_return: bool,
    pub auto_bounded_requeue: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDisposition {
    Ineligible,
    Manual(RetryTrigger),
    Automatic(RetryTrigger),
}

pub const fn retry_trigger(verdict: Verdict) -> Option<RetryTrigger> {
    match verdict {
        Verdict::PoolVanished => Some(RetryTrigger::PoolReturn),
        Verdict::Preempted => Some(RetryTrigger::ResourceReturn),
        Verdict::RuntimeExceeded => Some(RetryTrigger::BoundedRequeue),
        Verdict::Pass
        | Verdict::CleanExitNoArtifact
        | Verdict::Failed
        | Verdict::Cancelled
        | Verdict::Reused
        | Verdict::Substituted => None,
    }
}

pub const fn retry_disposition(verdict: Verdict, policy: RetryPolicy) -> RetryDisposition {
    let Some(trigger) = retry_trigger(verdict) else {
        return RetryDisposition::Ineligible;
    };
    let automatic = match trigger {
        RetryTrigger::PoolReturn => policy.auto_pool_return,
        RetryTrigger::ResourceReturn => policy.auto_resource_return,
        RetryTrigger::BoundedRequeue => policy.auto_bounded_requeue,
    };
    if automatic {
        RetryDisposition::Automatic(trigger)
    } else {
        RetryDisposition::Manual(trigger)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeSet;
    use std::ffi::CString;
    use std::fs;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::fs::PermissionsExt;

    use serde_json::Value;

    use super::*;
    use crate::taskdb::{AdmissionOrigin, EnqueueSource};
    use crate::witness::{
        counts_toward_canonical_gpu_seconds, LaborClass, RecordType, WitnessRecord,
        WITNESS_SCHEMA_VERSION,
    };

    fn parse(specs: &[&str]) -> EvidenceSpec {
        EvidenceSpec::parse(specs.iter().copied()).unwrap()
    }

    #[derive(Default)]
    struct FakeStore {
        valid: BTreeSet<PathBuf>,
        calls: RefCell<Vec<PathBuf>>,
    }

    impl FakeStore {
        fn with_valid(paths: impl IntoIterator<Item = &'static str>) -> Self {
            Self {
                valid: paths.into_iter().map(PathBuf::from).collect(),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl StoreValidity for FakeStore {
        fn check_validity(&self, path: &Path) -> Result<(), String> {
            self.calls.borrow_mut().push(path.to_owned());
            if self.valid.contains(path) {
                Ok(())
            } else {
                Err(format!("{} is invalid or unavailable", path.display()))
            }
        }
    }

    fn witness_record(
        dedup_key: &str,
        verdict: Verdict,
        artifact_content_hash: Option<String>,
        seq: u64,
    ) -> WitnessRecord {
        WitnessRecord {
            schema_version: WITNESS_SCHEMA_VERSION,
            record_type: RecordType::Verdict,
            transition_timestamp: "2026-07-19T00:00:00.000Z".to_owned(),
            task_uuid: None,
            verdict,
            exit_code: 0,
            artifact_content_hash,
            store_paths: None,
            drv: None,
            gpu_seconds: Some(1.0),
            wall_clock: 1.0,
            attempt: 1,
            lease_epoch: 1,
            dedup_key: Some(dedup_key.to_owned()),
            payload_hash: None,
            brief_hash: None,
            origin: AdmissionOrigin::direct(EnqueueSource::Manual),
            orchestration: None,
            labor_class: LaborClass::Fresh,
            trace_ref: None,
            pools: vec!["gpu".to_owned()],
            executor: None,
            host_id: None,
            charge: None,
            model: None,
            evidence_class: None,
            manifest_hash: None,
            completion: None,
            result_revision: None,
            authorship: None,
            authorship_sessions: None,
            extensions: serde_json::Map::new(),
            seq,
            prev_hash: String::new(),
            hash: String::new(),
        }
    }

    #[test]
    fn evidence_specs_round_trip_canonically_and_reject_ambiguity() {
        let upper_hash = "A".repeat(64);
        let spec = parse(&[
            "artifact:/tmp/a:file",
            &format!("hash:sha256:{upper_hash}"),
            "exit:7",
        ]);
        assert_eq!(
            spec.render(),
            [
                "artifact:/tmp/a:file",
                &format!("hash:sha256:{}", "a".repeat(64)),
                "exit:7",
            ]
        );

        for invalid in [
            vec!["artifact:"],
            vec!["artifact:relative/result.json"],
            vec!["hash:md5"],
            vec!["hash:sha256:short"],
            vec!["exit:not-a-number"],
            vec!["exit:256"],
            vec!["unknown:value"],
            vec!["artifact:/a", "hash:sha256", "hash:sha256"],
            vec!["exit:0", "exit:1"],
        ] {
            assert!(EvidenceSpec::parse(invalid).is_err());
        }
        assert!(EvidenceSpec::new(vec![EvidenceCheck::Artifact(PathBuf::new())]).is_err());
        assert!(
            EvidenceSpec::new(vec![EvidenceCheck::Artifact(PathBuf::from(
                "relative/result.json",
            ))])
            .is_err()
        );
        assert!(
            EvidenceSpec::new(vec![EvidenceCheck::Artifact(PathBuf::from(
                std::ffi::OsString::from_vec(vec![0xff]),
            ))])
            .is_err()
        );
        assert!(EvidenceSpec::new(vec![EvidenceCheck::HashSha256 {
            expected: Some("short".to_owned()),
        }])
        .is_err());
        assert!(EvidenceSpec::new(vec![EvidenceCheck::Exit(-1)]).is_err());
    }

    #[test]
    fn store_specs_use_the_exact_nix_shape_and_reject_duplicates() {
        const FIRST: &str = "/nix/store/00000000000000000000000000000000-first+1.0";
        const SECOND: &str = "/nix/store/11111111111111111111111111111111-second_out?x=y";
        let spec = parse(&[&format!("store:{SECOND}"), &format!("store:{FIRST}")]);
        assert_eq!(
            spec.render(),
            [format!("store:{SECOND}"), format!("store:{FIRST}")]
        );
        assert!(EvidenceSpec::parse(
            [format!("store:{FIRST}"), format!("store:{FIRST}")]
                .iter()
                .map(String::as_str)
        )
        .is_err());
        for invalid in [
            "store:/tmp/not-store",
            "store:/nix/store/0000000000000000000000000000000-short",
            "store:/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-bad-alphabet",
            "store:/nix/store/00000000000000000000000000000000-name/child",
            "store:/nix/store/00000000000000000000000000000000-",
        ] {
            assert!(
                invalid.parse::<EvidenceCheck>().is_err(),
                "{invalid:?} parsed"
            );
        }
    }

    #[test]
    fn store_gate_checks_each_path_once_sorts_passes_and_fails_closed() {
        const FIRST: &str = "/nix/store/00000000000000000000000000000000-first";
        const SECOND: &str = "/nix/store/11111111111111111111111111111111-second";
        let evidence = parse(&[&format!("store:{SECOND}"), &format!("store:{FIRST}")]);
        let store = FakeStore::with_valid([FIRST, SECOND]);
        let result = run_evidence_gate_with_store(
            RunOutcome {
                exit_code: 0,
                wall_clock_seconds: 0.25,
                evidence: &evidence,
            },
            &store,
        );
        assert_eq!(result.verdict, Verdict::Pass);
        assert_eq!(result.artifact_hash, None);
        assert_eq!(
            result.store_paths,
            Some(vec![FIRST.to_owned(), SECOND.to_owned()])
        );
        assert_eq!(
            store.calls.borrow().as_slice(),
            [PathBuf::from(SECOND), PathBuf::from(FIRST)]
        );

        let store = FakeStore::with_valid([FIRST]);
        let result = run_evidence_gate_with_store(
            RunOutcome {
                exit_code: 0,
                wall_clock_seconds: 0.25,
                evidence: &evidence,
            },
            &store,
        );
        assert_eq!(result.verdict, Verdict::CleanExitNoArtifact);
        assert_eq!(result.store_paths, Some(vec![FIRST.to_owned()]));
        assert_eq!(store.calls.borrow().len(), 2);
    }

    #[test]
    fn store_only_dedup_revalidates_and_requires_witness_set_equality() {
        const FIRST: &str = "/nix/store/00000000000000000000000000000000-first";
        const SECOND: &str = "/nix/store/11111111111111111111111111111111-second";
        let evidence = parse(&[&format!("store:{SECOND}"), &format!("store:{FIRST}")]);
        let mut record = witness_record("store-only", Verdict::Pass, None, 9);
        record.store_paths = Some(vec![FIRST.to_owned(), SECOND.to_owned()]);
        let store = FakeStore::with_valid([FIRST, SECOND]);
        let hit = probe_dedup_with_store(
            Some("store-only"),
            &evidence,
            std::slice::from_ref(&record),
            &store,
        );
        assert!(hit.hit);
        assert_eq!(hit.artifact_hash, None);
        assert_eq!(hit.store_paths, record.store_paths);
        assert_eq!(store.calls.borrow().len(), 2);

        let store = FakeStore::with_valid([FIRST]);
        assert_eq!(
            probe_full_pass_with_store(&evidence, &record, &store).miss_reason,
            Some(DedupMissReason::StorePathInvalid(PathBuf::from(SECOND)))
        );

        let store = FakeStore::with_valid([FIRST, SECOND]);
        record.store_paths = Some(vec![FIRST.to_owned()]);
        assert_eq!(
            probe_full_pass_with_store(&evidence, &record, &store).miss_reason,
            Some(DedupMissReason::WitnessStorePathsMismatch)
        );
    }

    #[test]
    fn gate_passes_existing_artifacts_and_uses_the_canonical_combiner() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        fs::write(&first, b"AAA").unwrap();
        fs::write(&second, b"BBB").unwrap();
        let evidence = parse(&[
            &format!("artifact:{}", first.display()),
            &format!("artifact:{}", second.display()),
            "hash:sha256",
            "exit:0",
        ]);

        let first_hash = hash_artifact_file(&first).unwrap();
        let second_hash = hash_artifact_file(&second).unwrap();
        assert_eq!(
            first_hash,
            "sha256:cb1ad2119d8fafb69566510ee712661f9f14b83385006ef92aec47f523a38358"
        );
        assert_eq!(
            second_hash,
            "sha256:dcdb704109a454784b81229d2b05f368692e758bfa33cb61d04c1b93791b0273"
        );
        let expected = "sha256:b12e3af37a33b58ca4f9a6f71e71024fc02087f2b0bc952e9e30d6776e32bf66";
        assert_eq!(
            combine_artifact_hashes(&[first_hash.clone(), second_hash.clone()]).as_deref(),
            Some(expected)
        );
        assert_ne!(
            combine_artifact_hashes(&[second_hash, first_hash]).as_deref(),
            Some(expected)
        );
        let result = run_evidence_gate(RunOutcome {
            exit_code: 0,
            wall_clock_seconds: 1.25,
            evidence: &evidence,
        });

        assert!(result.passed);
        assert_eq!(result.verdict, Verdict::Pass);
        assert_eq!(result.artifact_hash.as_deref(), Some(expected));
        assert_eq!(checked_paths(&result).last().unwrap(), "hash:sha256");
    }

    #[test]
    fn single_artifact_hash_is_the_leaf_hash() {
        let hash = format!("sha256:{}", "a".repeat(64));
        assert_eq!(
            combine_artifact_hashes(std::slice::from_ref(&hash)),
            Some(hash)
        );
        assert_eq!(combine_artifact_hashes(&[]), None);
    }

    #[test]
    fn gate_distinguishes_missing_artifact_bad_exit_and_bad_span() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing");
        let evidence = parse(&[&format!("artifact:{}", missing.display()), "exit:0"]);
        let missing_result = run_evidence_gate(RunOutcome {
            exit_code: 0,
            wall_clock_seconds: 1.0,
            evidence: &evidence,
        });
        assert_eq!(missing_result.verdict, Verdict::CleanExitNoArtifact);
        assert!(missing_result.clean_exit_no_artifact);
        assert_eq!(missing_result.artifact_hash, None);

        let failed_result = run_evidence_gate(RunOutcome {
            exit_code: 7,
            wall_clock_seconds: 1.0,
            evidence: &evidence,
        });
        assert_eq!(failed_result.verdict, Verdict::Failed);
        assert!(!failed_result.clean_exit_no_artifact);

        for wall_clock_seconds in [-0.01, f64::NAN] {
            let bad_span = run_evidence_gate(RunOutcome {
                exit_code: 0,
                wall_clock_seconds,
                evidence: &evidence,
            });
            assert_eq!(bad_span.verdict, Verdict::Failed);
        }
    }

    #[test]
    fn gate_rejects_directory_and_fixed_hash_mismatch() {
        let temp = tempfile::tempdir().unwrap();
        let directory_evidence = parse(&[&format!("artifact:{}", temp.path().display())]);
        assert_eq!(
            run_evidence_gate(RunOutcome {
                exit_code: 0,
                wall_clock_seconds: 0.0,
                evidence: &directory_evidence,
            })
            .verdict,
            Verdict::CleanExitNoArtifact
        );

        let artifact = temp.path().join("artifact");
        fs::write(&artifact, b"content").unwrap();
        let other_hash = format!("hash:sha256:{}", "0".repeat(64));
        let mismatch = parse(&[&format!("artifact:{}", artifact.display()), &other_hash]);
        assert_eq!(
            run_evidence_gate(RunOutcome {
                exit_code: 0,
                wall_clock_seconds: 0.0,
                evidence: &mismatch,
            })
            .verdict,
            Verdict::CleanExitNoArtifact
        );
    }

    #[test]
    fn hashing_fails_closed_when_the_open_artifact_changes_or_is_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let artifact = temp.path().join("artifact");
        fs::write(&artifact, b"original").unwrap();
        let error = hash_artifact_file_with_hook(&artifact, || {
            fs::write(&artifact, b"changed and a different size").unwrap();
        })
        .unwrap_err();
        assert!(matches!(error, EvidenceError::ArtifactChanged(path) if path == artifact));

        fs::write(&artifact, b"original").unwrap();
        let replacement = temp.path().join("replacement");
        fs::write(&replacement, b"replacement").unwrap();
        let error = hash_artifact_file_with_hook(&artifact, || {
            fs::rename(&replacement, &artifact).unwrap();
        })
        .unwrap_err();
        assert!(matches!(error, EvidenceError::ArtifactChanged(path) if path == artifact));
    }

    #[test]
    fn hashing_rejects_a_fifo_without_blocking() {
        let temp = tempfile::tempdir().unwrap();
        let fifo = temp.path().join("artifact.fifo");
        let path = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: `path` is a live NUL-terminated pathname and the mode contains only permission bits.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert!(matches!(
            hash_artifact_file(&fifo),
            Err(EvidenceError::NotRegularFile(path)) if path == fifo
        ));
    }

    #[test]
    fn gate_and_dedup_fail_closed_for_an_unreadable_regular_file() {
        // Root can bypass Unix permission bits; the ordinary and Nix sandbox suites run unprivileged.
        // SAFETY: `geteuid` has no preconditions and does not mutate process state.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let artifact = temp.path().join("unreadable");
        fs::write(&artifact, b"content").unwrap();
        let hash = hash_artifact_file(&artifact).unwrap();
        let evidence = parse(&[&format!("artifact:{}", artifact.display())]);
        let record = witness_record("unreadable", Verdict::Pass, Some(hash), 1);
        fs::set_permissions(&artifact, fs::Permissions::from_mode(0o000)).unwrap();

        let gate = run_evidence_gate(RunOutcome {
            exit_code: 0,
            wall_clock_seconds: 1.0,
            evidence: &evidence,
        });
        assert_eq!(gate.verdict, Verdict::CleanExitNoArtifact);
        assert_eq!(
            probe_dedup(Some("unreadable"), &evidence, &[record]).miss_reason,
            Some(DedupMissReason::ArtifactUnavailable(artifact.clone()))
        );

        fs::set_permissions(&artifact, fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn empty_evidence_passes_on_the_exit_and_span_floor() {
        let evidence = EvidenceSpec::default();
        let result = run_evidence_gate(RunOutcome {
            exit_code: 0,
            wall_clock_seconds: 0.0,
            evidence: &evidence,
        });
        assert_eq!(result.verdict, Verdict::Pass);
        assert_eq!(result.artifact_hash, None);
    }

    #[test]
    fn multi_artifact_dedup_uses_the_same_ordered_combined_hash() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        fs::write(&first, b"AAA").unwrap();
        fs::write(&second, b"BBB").unwrap();
        let evidence = parse(&[
            &format!("artifact:{}", first.display()),
            &format!("artifact:{}", second.display()),
            "exit:0",
        ]);
        let gate = run_evidence_gate(RunOutcome {
            exit_code: 0,
            wall_clock_seconds: 1.0,
            evidence: &evidence,
        });
        let records = vec![witness_record(
            "multi-1",
            Verdict::Pass,
            gate.artifact_hash.clone(),
            7,
        )];

        let result = probe_dedup(Some("multi-1"), &evidence, &records);
        assert!(result.hit);
        assert!(result.rehashed);
        assert_eq!(result.artifact_hash, gate.artifact_hash);
        assert_eq!(result.matched_witness_seq, Some(7));
    }

    #[test]
    fn dedup_misses_changed_missing_keyless_and_non_success_work() {
        let temp = tempfile::tempdir().unwrap();
        let artifact = temp.path().join("artifact");
        fs::write(&artifact, b"before").unwrap();
        let evidence = parse(&[&format!("artifact:{}", artifact.display())]);
        let original_hash = hash_artifact_file(&artifact).unwrap();
        let pass = witness_record("same", Verdict::Pass, Some(original_hash), 1);

        assert_eq!(
            probe_dedup(None, &evidence, std::slice::from_ref(&pass)).miss_reason,
            Some(DedupMissReason::NoKey)
        );
        fs::write(&artifact, b"after").unwrap();
        assert_eq!(
            probe_dedup(Some("same"), &evidence, std::slice::from_ref(&pass)).miss_reason,
            Some(DedupMissReason::WitnessHashMismatch)
        );
        fs::remove_file(&artifact).unwrap();
        assert_eq!(
            probe_dedup(Some("same"), &evidence, std::slice::from_ref(&pass)).miss_reason,
            Some(DedupMissReason::ArtifactUnavailable(artifact.clone()))
        );

        let failed = witness_record(
            "same",
            Verdict::CleanExitNoArtifact,
            Some(format!("sha256:{}", "f".repeat(64))),
            2,
        );
        assert_eq!(
            probe_dedup(Some("same"), &evidence, &[failed]).miss_reason,
            Some(DedupMissReason::NoPriorPass)
        );
    }

    #[test]
    fn dedup_uses_newest_pass_and_honors_a_fixed_declared_hash() {
        let temp = tempfile::tempdir().unwrap();
        let artifact = temp.path().join("artifact");
        fs::write(&artifact, b"current").unwrap();
        let current = hash_artifact_file(&artifact).unwrap();
        let stale = format!("sha256:{}", "0".repeat(64));
        let records = vec![
            witness_record("same", Verdict::Pass, Some(stale), 2),
            witness_record("same", Verdict::Pass, Some(current.clone()), 1),
        ];
        let evidence = parse(&[&format!("artifact:{}", artifact.display())]);
        assert_eq!(
            probe_dedup(Some("same"), &evidence, &records).miss_reason,
            Some(DedupMissReason::WitnessHashMismatch)
        );

        let wrong_declared = parse(&[
            &format!("artifact:{}", artifact.display()),
            &format!("hash:sha256:{}", "f".repeat(64)),
        ]);
        assert_eq!(
            probe_dedup(
                Some("same"),
                &wrong_declared,
                &[witness_record("same", Verdict::Pass, Some(current), 3)]
            )
            .miss_reason,
            Some(DedupMissReason::DeclaredHashMismatch)
        );
    }

    #[test]
    fn dedup_requires_artifact_evidence_and_a_readable_regular_file() {
        let hash = format!("sha256:{}", "a".repeat(64));
        let record = witness_record("same", Verdict::Pass, Some(hash), 1);
        assert_eq!(
            probe_dedup(
                Some("same"),
                &EvidenceSpec::default(),
                std::slice::from_ref(&record)
            )
            .miss_reason,
            Some(DedupMissReason::NoArtifactEvidence)
        );

        let temp = tempfile::tempdir().unwrap();
        let evidence = parse(&[&format!("artifact:{}", temp.path().display())]);
        assert_eq!(
            probe_dedup(Some("same"), &evidence, &[record]).miss_reason,
            Some(DedupMissReason::ArtifactUnavailable(temp.path().to_owned()))
        );
    }

    #[test]
    fn multi_artifact_dedup_misses_if_either_artifact_changes_or_disappears() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        fs::write(&first, b"first").unwrap();
        fs::write(&second, b"second").unwrap();
        let evidence = parse(&[
            &format!("artifact:{}", first.display()),
            &format!("artifact:{}", second.display()),
        ]);
        let witnessed = combine_artifact_hashes(&[
            hash_artifact_file(&first).unwrap(),
            hash_artifact_file(&second).unwrap(),
        ]);
        let record = witness_record("multi", Verdict::Pass, witnessed, 1);
        assert!(probe_dedup(Some("multi"), &evidence, std::slice::from_ref(&record)).hit);

        fs::write(&second, b"changed").unwrap();
        assert_eq!(
            probe_dedup(Some("multi"), &evidence, std::slice::from_ref(&record)).miss_reason,
            Some(DedupMissReason::WitnessHashMismatch)
        );
        fs::remove_file(&first).unwrap();
        assert_eq!(
            probe_dedup(Some("multi"), &evidence, &[record]).miss_reason,
            Some(DedupMissReason::ArtifactUnavailable(first))
        );
    }

    #[test]
    fn retry_policy_keeps_eligibility_and_automatic_choice_distinct() {
        let manual = RetryPolicy {
            auto_pool_return: false,
            auto_resource_return: false,
            auto_bounded_requeue: false,
        };
        let automatic = RetryPolicy {
            auto_pool_return: true,
            auto_resource_return: true,
            auto_bounded_requeue: true,
        };
        for (verdict, trigger) in [
            (Verdict::PoolVanished, RetryTrigger::PoolReturn),
            (Verdict::Preempted, RetryTrigger::ResourceReturn),
            (Verdict::RuntimeExceeded, RetryTrigger::BoundedRequeue),
        ] {
            assert_eq!(
                retry_disposition(verdict, manual),
                RetryDisposition::Manual(trigger)
            );
            assert_eq!(
                retry_disposition(verdict, automatic),
                RetryDisposition::Automatic(trigger)
            );
        }
        for verdict in [
            Verdict::Pass,
            Verdict::CleanExitNoArtifact,
            Verdict::Failed,
            Verdict::Cancelled,
            Verdict::Reused,
        ] {
            assert_eq!(
                retry_disposition(verdict, automatic),
                RetryDisposition::Ineligible
            );
        }
    }

    #[test]
    fn canonical_gpu_metering_preserves_consumed_fresh_attempts_only() {
        let mut record = witness_record(
            "meter",
            Verdict::Pass,
            Some(format!("sha256:{}", "a".repeat(64))),
            1,
        );
        assert!(counts_toward_canonical_gpu_seconds(&record));

        record.verdict = Verdict::Failed;
        assert!(counts_toward_canonical_gpu_seconds(&record));
        record.verdict = Verdict::RuntimeExceeded;
        assert!(counts_toward_canonical_gpu_seconds(&record));

        for verdict in [
            Verdict::CleanExitNoArtifact,
            Verdict::Cancelled,
            Verdict::PoolVanished,
            Verdict::Preempted,
        ] {
            record.verdict = verdict;
            assert!(!counts_toward_canonical_gpu_seconds(&record));
        }

        record.verdict = Verdict::Pass;
        for labor_class in [LaborClass::Recovered, LaborClass::Reused] {
            record.labor_class = labor_class;
            assert!(!counts_toward_canonical_gpu_seconds(&record));
        }
    }

    #[test]
    fn opaque_witness_fields_are_irrelevant_to_dedup_matching() {
        let temp = tempfile::tempdir().unwrap();
        let artifact = temp.path().join("artifact");
        fs::write(&artifact, b"content").unwrap();
        let evidence = parse(&[&format!("artifact:{}", artifact.display())]);
        let mut record = witness_record(
            "same",
            Verdict::Pass,
            Some(hash_artifact_file(&artifact).unwrap()),
            1,
        );
        record.evidence_class = Some(Value::String("capture".to_owned()));
        record.manifest_hash = Some(Value::String("opaque".to_owned()));
        assert!(probe_dedup(Some("same"), &evidence, &[record]).hit);
    }
}
