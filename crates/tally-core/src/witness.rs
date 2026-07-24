use std::collections::{BTreeSet, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::completion::SemanticCompletion;

pub const GENESIS_PREV_HASH: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Pass,
    CleanExitNoArtifact,
    Failed,
    Cancelled,
    Reused,
    PoolVanished,
    Preempted,
    RuntimeExceeded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LaborClass {
    Fresh,
    Recovered,
    Reused,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Charge {
    pub unit: String,
    pub amount: f64,
    #[serde(rename = "class")]
    pub class_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WitnessRecord {
    pub task_uuid: Option<String>,
    pub transition_timestamp: String,
    pub verdict: Verdict,
    pub exit_code: i32,
    pub artifact_content_hash: Option<String>,
    pub gpu_seconds: Option<f64>,
    pub wall_clock: f64,
    pub attempt: u32,
    pub lease_epoch: u64,
    pub dedup_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_hash: Option<String>,
    pub labor_class: LaborClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_ref: Option<String>,
    #[serde(
        rename = "pool",
        serialize_with = "crate::poolset::serialize_optional",
        deserialize_with = "crate::poolset::deserialize_optional"
    )]
    pub pools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<String>,
    pub charge: Option<Charge>,
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_class: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_hash: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<SemanticCompletion>,
    pub seq: u64,
    pub prev_hash: String,
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WitnessBody {
    pub task_uuid: Option<String>,
    pub transition_timestamp: String,
    pub verdict: Verdict,
    pub exit_code: i32,
    pub artifact_content_hash: Option<String>,
    pub gpu_seconds: Option<f64>,
    pub wall_clock: f64,
    pub attempt: u32,
    pub lease_epoch: u64,
    pub dedup_key: Option<String>,
    pub payload_hash: Option<String>,
    pub labor_class: LaborClass,
    pub trace_ref: Option<String>,
    pub pools: Option<Vec<String>>,
    pub executor: Option<String>,
    pub charge: Option<Charge>,
    pub model: Option<String>,
    pub evidence_class: Option<Value>,
    pub manifest_hash: Option<Value>,
    pub completion: Option<SemanticCompletion>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainHead {
    pub seq: u64,
    pub hash: String,
}

impl Default for ChainHead {
    fn default() -> Self {
        Self {
            seq: 0,
            hash: GENESIS_PREV_HASH.to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerifyProblemKind {
    ParseError,
    InvalidRecord,
    HashMismatch,
    PrevHashMismatch,
    SeqOrder,
    SeqGap,
    SeqDuplicate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifyProblem {
    pub seq: Option<u64>,
    pub line: usize,
    pub kind: VerifyProblemKind,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifyReport {
    pub ok: bool,
    pub records: usize,
    #[serde(rename = "firstSeq")]
    pub first_seq: Option<u64>,
    #[serde(rename = "lastSeq")]
    pub last_seq: Option<u64>,
    pub problems: Vec<VerifyProblem>,
}

#[derive(Debug, Error)]
pub enum WitnessError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("ledger is corrupt: {0}")]
    Corrupt(String),
    #[error("cannot serialize ledger record: {0}")]
    Json(#[from] serde_json::Error),
}

fn io_error(path: &Path, source: std::io::Error) -> WitnessError {
    WitnessError::Io {
        path: path.to_owned(),
        source,
    }
}

pub fn canonical_hash_input(raw: &Value) -> Result<String, WitnessError> {
    let mut cleared = raw.clone();
    let object = cleared
        .as_object_mut()
        .ok_or_else(|| WitnessError::Corrupt("record is not a JSON object".to_owned()))?;
    if !object.contains_key("hash") {
        return Err(WitnessError::Corrupt("record has no hash field".to_owned()));
    }
    object.insert("hash".to_owned(), Value::String(String::new()));
    Ok(serde_json::to_string(&cleared)?)
}

pub fn compute_hash_value(raw: &Value) -> Result<String, WitnessError> {
    let input = canonical_hash_input(raw)?;
    let digest = Sha256::digest(input.as_bytes());
    Ok(format!("sha256:{digest:x}"))
}

pub fn compute_hash(record: &WitnessRecord) -> Result<String, WitnessError> {
    compute_hash_value(&serde_json::to_value(record)?)
}

pub fn build_record(body: WitnessBody, head: &ChainHead) -> Result<WitnessRecord, WitnessError> {
    let mut pools = body.pools;
    if let Some(pools) = &mut pools {
        crate::poolset::canonicalize(pools)
            .map_err(|error| WitnessError::Corrupt(error.to_string()))?;
    }
    let mut record = WitnessRecord {
        task_uuid: body.task_uuid,
        transition_timestamp: body.transition_timestamp,
        verdict: body.verdict,
        exit_code: body.exit_code,
        artifact_content_hash: body.artifact_content_hash,
        gpu_seconds: body.gpu_seconds,
        wall_clock: body.wall_clock,
        attempt: body.attempt,
        lease_epoch: body.lease_epoch,
        dedup_key: body.dedup_key,
        payload_hash: body.payload_hash,
        labor_class: body.labor_class,
        trace_ref: body.trace_ref,
        pools,
        executor: body.executor,
        charge: body.charge,
        model: body.model,
        evidence_class: body.evidence_class,
        manifest_hash: body.manifest_hash,
        completion: body.completion,
        seq: head.seq + 1,
        prev_hash: head.hash.clone(),
        hash: String::new(),
    };
    record.hash = compute_hash(&record)?;
    Ok(record)
}

fn sha256_shape(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_record(raw: &Value) -> Result<WitnessRecord, String> {
    let object = raw
        .as_object()
        .ok_or_else(|| "line is not a JSON object".to_owned())?;
    for forbidden in ["parent", "parent_uuid", "kind"] {
        if object.contains_key(forbidden) {
            return Err(format!("{forbidden} is not a canonical witness field"));
        }
    }
    let record: WitnessRecord =
        serde_json::from_value(raw.clone()).map_err(|error| error.to_string())?;
    if let Some(pools) = &record.pools {
        let mut canonical = pools.clone();
        crate::poolset::canonicalize(&mut canonical).map_err(|error| error.to_string())?;
        if &canonical != pools {
            return Err("pool set is not in canonical order".to_owned());
        }
    }
    if record.executor.as_ref().is_some_and(|executor| {
        executor.is_empty()
            || executor.len() > 96
            || matches!(executor.as_str(), "." | "..")
            || !executor
                .as_bytes()
                .first()
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            || !executor
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
    }) {
        return Err("executor is not a safe registry component".to_owned());
    }
    if record.payload_hash.as_ref().is_some_and(|payload_hash| {
        payload_hash.len() != 71
            || !payload_hash.starts_with("sha256:")
            || !payload_hash[7..]
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }) {
        return Err("payload_hash is not lowercase sha256 hex".to_owned());
    }
    if record.seq == 0 {
        return Err("seq missing or not a positive integer".to_owned());
    }
    if !sha256_shape(&record.prev_hash) {
        return Err("prev_hash missing or not a sha256: hash".to_owned());
    }
    if !sha256_shape(&record.hash) {
        return Err("hash missing or not a sha256: hash".to_owned());
    }
    if !record.wall_clock.is_finite()
        || record.gpu_seconds.is_some_and(|value| !value.is_finite())
        || record
            .charge
            .as_ref()
            .is_some_and(|charge| !charge.amount.is_finite())
    {
        return Err("numeric fields must be finite".to_owned());
    }
    Ok(record)
}

#[derive(Debug)]
struct ParsedRecord {
    record: WitnessRecord,
    raw: Value,
    line: usize,
}

pub fn verify_reader(reader: impl BufRead) -> VerifyReport {
    let mut problems = Vec::new();
    let mut valid = Vec::new();

    for (index, line_result) in reader.lines().enumerate() {
        let line_number = index + 1;
        let line = match line_result {
            Ok(line) => line,
            Err(error) => {
                problems.push(VerifyProblem {
                    seq: None,
                    line: line_number,
                    kind: VerifyProblemKind::ParseError,
                    reason: format!("cannot read line: {error}"),
                });
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let raw: Value = match serde_json::from_str(&line) {
            Ok(raw) => raw,
            Err(_) => {
                problems.push(VerifyProblem {
                    seq: None,
                    line: line_number,
                    kind: VerifyProblemKind::ParseError,
                    reason: "line is not valid JSON".to_owned(),
                });
                continue;
            }
        };
        match validate_record(&raw) {
            Ok(record) => valid.push(ParsedRecord {
                record,
                raw,
                line: line_number,
            }),
            Err(reason) => problems.push(VerifyProblem {
                seq: raw.get("seq").and_then(Value::as_u64),
                line: line_number,
                kind: VerifyProblemKind::InvalidRecord,
                reason,
            }),
        }
    }

    for parsed in &valid {
        match compute_hash_value(&parsed.raw) {
            Ok(recomputed) if recomputed != parsed.record.hash => problems.push(VerifyProblem {
                seq: Some(parsed.record.seq),
                line: parsed.line,
                kind: VerifyProblemKind::HashMismatch,
                reason: format!(
                    "stored hash {} != recomputed {} (line tampered)",
                    parsed.record.hash, recomputed
                ),
            }),
            Err(error) => problems.push(VerifyProblem {
                seq: Some(parsed.record.seq),
                line: parsed.line,
                kind: VerifyProblemKind::InvalidRecord,
                reason: error.to_string(),
            }),
            _ => {}
        }
    }

    let mut previous_hash = GENESIS_PREV_HASH;
    for parsed in &valid {
        if parsed.record.prev_hash != previous_hash {
            problems.push(VerifyProblem {
                seq: Some(parsed.record.seq),
                line: parsed.line,
                kind: VerifyProblemKind::PrevHashMismatch,
                reason: format!(
                    "prev_hash {} != predecessor hash {} (chain broken)",
                    parsed.record.prev_hash, previous_hash
                ),
            });
        }
        previous_hash = &parsed.record.hash;
    }

    let mut previous_seq = 0;
    for parsed in &valid {
        if parsed.record.seq <= previous_seq {
            problems.push(VerifyProblem {
                seq: Some(parsed.record.seq),
                line: parsed.line,
                kind: VerifyProblemKind::SeqOrder,
                reason: format!(
                    "seq {} does not strictly follow {} (reordered or duplicate)",
                    parsed.record.seq, previous_seq
                ),
            });
        }
        previous_seq = parsed.record.seq;
    }

    let mut sequence_lines = HashMap::new();
    let mut sequences = BTreeSet::new();
    for parsed in &valid {
        if !sequences.insert(parsed.record.seq) {
            problems.push(VerifyProblem {
                seq: Some(parsed.record.seq),
                line: parsed.line,
                kind: VerifyProblemKind::SeqDuplicate,
                reason: format!("seq {} appears more than once", parsed.record.seq),
            });
        }
        sequence_lines.insert(parsed.record.seq, parsed.line);
    }
    for (offset, sequence) in sequences.iter().enumerate() {
        let expected = offset as u64 + 1;
        if *sequence != expected {
            problems.push(VerifyProblem {
                seq: Some(*sequence),
                line: sequence_lines[sequence],
                kind: VerifyProblemKind::SeqGap,
                reason: format!("expected seq {expected} but found {sequence} (missing line)"),
            });
            break;
        }
    }

    VerifyReport {
        ok: problems.is_empty(),
        records: valid.len(),
        first_seq: valid.first().map(|parsed| parsed.record.seq),
        last_seq: valid.last().map(|parsed| parsed.record.seq),
        problems,
    }
}

pub fn verify_file(path: &Path) -> Result<VerifyReport, WitnessError> {
    if !path.exists() {
        return Ok(VerifyReport {
            ok: true,
            records: 0,
            first_seq: None,
            last_seq: None,
            problems: Vec::new(),
        });
    }
    let file = File::open(path).map_err(|source| io_error(path, source))?;
    Ok(verify_reader(BufReader::new(file)))
}

pub fn read_verified_records(
    path: &Path,
) -> Result<(VerifyReport, Vec<WitnessRecord>), WitnessError> {
    if !path.exists() {
        return Ok((
            VerifyReport {
                ok: true,
                records: 0,
                first_seq: None,
                last_seq: None,
                problems: Vec::new(),
            },
            Vec::new(),
        ));
    }
    let mut file = File::open(path).map_err(|source| io_error(path, source))?;
    file.lock_shared()
        .map_err(|source| io_error(path, source))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    let report = verify_reader(Cursor::new(bytes.as_slice()));
    if !report.ok {
        return Ok((report, Vec::new()));
    }
    let records = BufReader::new(Cursor::new(bytes))
        .lines()
        .filter_map(|line| match line {
            Ok(line) if line.trim().is_empty() => None,
            other => Some(other),
        })
        .map(|line| {
            let line = line.map_err(|source| io_error(path, source))?;
            let raw = serde_json::from_str(&line)?;
            validate_record(&raw).map_err(WitnessError::Corrupt)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((report, records))
}

pub fn read_records(path: &Path) -> Result<Vec<WitnessRecord>, WitnessError> {
    let file = File::open(path).map_err(|source| io_error(path, source))?;
    BufReader::new(file)
        .lines()
        .filter_map(|line| match line {
            Ok(line) if line.trim().is_empty() => None,
            other => Some(other),
        })
        .map(|line| {
            let line = line.map_err(|source| io_error(path, source))?;
            let raw = serde_json::from_str(&line)?;
            validate_record(&raw).map_err(WitnessError::Corrupt)
        })
        .collect()
}

pub fn counts_toward_canonical_gpu_seconds(record: &WitnessRecord) -> bool {
    record.labor_class == LaborClass::Fresh
        && !matches!(
            record.verdict,
            Verdict::CleanExitNoArtifact
                | Verdict::PoolVanished
                | Verdict::Preempted
                | Verdict::Cancelled
        )
}

pub fn canonical_gpu_seconds(records: impl IntoIterator<Item = WitnessRecord>) -> f64 {
    records
        .into_iter()
        .filter(counts_toward_canonical_gpu_seconds)
        .filter_map(|record| record.gpu_seconds)
        .sum()
}

fn scan_head(file: &mut File, path: &Path) -> Result<ChainHead, WitnessError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|source| io_error(path, source))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    if bytes.is_empty() {
        return Ok(ChainHead::default());
    }
    let complete_len = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |position| position + 1);
    if complete_len != bytes.len() {
        file.set_len(complete_len as u64)
            .map_err(|source| io_error(path, source))?;
        bytes.truncate(complete_len);
    }
    let report = verify_reader(BufReader::new(bytes.as_slice()));
    if !report.ok {
        return Err(WitnessError::Corrupt(
            serde_json::to_string(&report.problems)
                .unwrap_or_else(|_| "verification failed".to_owned()),
        ));
    }
    if report.records == 0 {
        return Ok(ChainHead::default());
    }
    let text = String::from_utf8(bytes)
        .map_err(|error| WitnessError::Corrupt(format!("ledger is not UTF-8: {error}")))?;
    let last = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .ok_or_else(|| WitnessError::Corrupt("ledger has no complete record".to_owned()))?;
    let raw: Value = serde_json::from_str(last)?;
    let record = validate_record(&raw).map_err(WitnessError::Corrupt)?;
    Ok(ChainHead {
        seq: record.seq,
        hash: record.hash,
    })
}

pub struct WitnessLedger {
    path: PathBuf,
    file: File,
    head: ChainHead,
}

impl WitnessLedger {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, WitnessError> {
        let path = path.as_ref().to_owned();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
        }
        let created = !path.exists();
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)
            .map_err(|source| io_error(&path, source))?;
        file.lock_exclusive()
            .map_err(|source| io_error(&path, source))?;
        let head = scan_head(&mut file, &path);
        let unlock = FileExt::unlock(&file).map_err(|source| io_error(&path, source));
        let head = head?;
        unlock?;
        if created {
            if let Some(parent) = path.parent() {
                File::open(parent)
                    .and_then(|directory| directory.sync_all())
                    .map_err(|source| io_error(parent, source))?;
            }
        }
        Ok(Self { path, file, head })
    }

    pub fn head(&self) -> &ChainHead {
        &self.head
    }

    pub fn append(&mut self, body: WitnessBody) -> Result<WitnessRecord, WitnessError> {
        self.file
            .lock_exclusive()
            .map_err(|source| io_error(&self.path, source))?;
        let result = (|| {
            self.head = scan_head(&mut self.file, &self.path)?;
            let record = build_record(body, &self.head)?;
            let mut line = serde_json::to_vec(&record)?;
            line.push(b'\n');
            self.file
                .write_all(&line)
                .map_err(|source| io_error(&self.path, source))?;
            self.file
                .sync_all()
                .map_err(|source| io_error(&self.path, source))?;
            self.head = ChainHead {
                seq: record.seq,
                hash: record.hash.clone(),
            };
            Ok(record)
        })();
        let unlock = FileExt::unlock(&self.file).map_err(|source| io_error(&self.path, source));
        match (result, unlock) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(record), Ok(())) => Ok(record),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttestationRecord {
    pub observed_at: String,
    pub payload: Value,
    pub seq: u64,
    pub prev_hash: String,
    pub hash: String,
}

pub fn append_attestation(path: &Path, payload: Value) -> Result<AttestationRecord, WitnessError> {
    let parent = durable_parent(path);
    std::fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)
        .map_err(|source| io_error(path, source))?;
    file.lock_exclusive()
        .map_err(|source| io_error(path, source))?;
    truncate_incomplete_attestation_tail(&mut file, path)?;
    let (seq, previous_hash) = scan_attestation_head(&mut file, path)?;
    let mut record = AttestationRecord {
        observed_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        payload,
        seq: seq + 1,
        prev_hash: previous_hash,
        hash: String::new(),
    };
    record.hash = compute_hash_value(&serde_json::to_value(&record)?)?;
    let mut line = serde_json::to_vec(&record)?;
    line.push(b'\n');
    file.write_all(&line)
        .map_err(|source| io_error(path, source))?;
    file.sync_all().map_err(|source| io_error(path, source))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error(parent, source))?;
    Ok(record)
}

fn durable_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

pub fn repair_attestation_tail(path: &Path) -> Result<(), WitnessError> {
    if !path.exists() {
        return Ok(());
    }
    let bytes = std::fs::read(path).map_err(|source| io_error(path, source))?;
    if bytes.is_empty() || bytes.last() == Some(&b'\n') {
        return Ok(());
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|source| io_error(path, source))?;
    file.lock_exclusive()
        .map_err(|source| io_error(path, source))?;
    if truncate_incomplete_attestation_tail(&mut file, path)? {
        file.sync_all().map_err(|source| io_error(path, source))?;
    }
    Ok(())
}

fn truncate_incomplete_attestation_tail(
    file: &mut File,
    path: &Path,
) -> Result<bool, WitnessError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|source| io_error(path, source))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    if bytes.is_empty() || bytes.last() == Some(&b'\n') {
        return Ok(false);
    }
    let complete_len = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |position| position + 1);
    if serde_json::from_slice::<Value>(&bytes[complete_len..]).is_ok() {
        scan_attestation_head(file, path)?;
        file.seek(SeekFrom::End(0))
            .map_err(|source| io_error(path, source))?;
        file.write_all(b"\n")
            .map_err(|source| io_error(path, source))?;
        return Ok(true);
    }
    file.set_len(complete_len as u64)
        .map_err(|source| io_error(path, source))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|source| io_error(path, source))?;
    Ok(true)
}

fn scan_attestation_head(file: &mut File, path: &Path) -> Result<(u64, String), WitnessError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|source| io_error(path, source))?;
    let reader = BufReader::new(file.try_clone().map_err(|source| io_error(path, source))?);
    let mut expected_seq = 1;
    let mut previous_hash = GENESIS_PREV_HASH.to_owned();
    for line in reader.lines() {
        let line = line.map_err(|source| io_error(path, source))?;
        if line.trim().is_empty() {
            continue;
        }
        let raw: Value = serde_json::from_str(&line)?;
        let record: AttestationRecord = serde_json::from_value(raw.clone())?;
        if record.seq != expected_seq
            || record.prev_hash != previous_hash
            || compute_hash_value(&raw)? != record.hash
        {
            return Err(WitnessError::Corrupt(format!(
                "attestation chain breaks at seq {}",
                record.seq
            )));
        }
        expected_seq += 1;
        previous_hash = record.hash;
    }
    Ok((expected_seq - 1, previous_hash))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttestationVerifyReport {
    pub ok: bool,
    pub records: usize,
    pub authentication: &'static str,
    pub problem: Option<String>,
}

pub fn verify_attestations(path: &Path) -> Result<AttestationVerifyReport, WitnessError> {
    if !path.exists() {
        return Ok(AttestationVerifyReport {
            ok: true,
            records: 0,
            authentication: "unauthenticated-by-construction",
            problem: None,
        });
    }
    let mut file = OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|source| io_error(path, source))?;
    match scan_attestation_head(&mut file, path) {
        Ok((records, _)) => Ok(AttestationVerifyReport {
            ok: true,
            records: records as usize,
            authentication: "unauthenticated-by-construction",
            problem: None,
        }),
        Err(WitnessError::Corrupt(problem)) => Ok(AttestationVerifyReport {
            ok: false,
            records: 0,
            authentication: "unauthenticated-by-construction",
            problem: Some(problem),
        }),
        Err(error) => Err(error),
    }
}

pub fn read_verified_attestations(
    path: &Path,
) -> Result<(AttestationVerifyReport, Vec<AttestationRecord>), WitnessError> {
    if !path.exists() {
        return Ok((
            AttestationVerifyReport {
                ok: true,
                records: 0,
                authentication: "unauthenticated-by-construction",
                problem: None,
            },
            Vec::new(),
        ));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|source| io_error(path, source))?;
    file.lock_shared()
        .map_err(|source| io_error(path, source))?;
    let report = match scan_attestation_head(&mut file, path) {
        Ok((records, _)) => AttestationVerifyReport {
            ok: true,
            records: records as usize,
            authentication: "unauthenticated-by-construction",
            problem: None,
        },
        Err(WitnessError::Corrupt(problem)) => {
            return Ok((
                AttestationVerifyReport {
                    ok: false,
                    records: 0,
                    authentication: "unauthenticated-by-construction",
                    problem: Some(problem),
                },
                Vec::new(),
            ))
        }
        Err(error) => return Err(error),
    };
    file.seek(SeekFrom::Start(0))
        .map_err(|source| io_error(path, source))?;
    let records = BufReader::new(file)
        .lines()
        .filter_map(|line| match line {
            Ok(line) if line.trim().is_empty() => None,
            other => Some(other),
        })
        .map(|line| {
            let line = line.map_err(|source| io_error(path, source))?;
            serde_json::from_str(&line).map_err(WitnessError::Json)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((report, records))
}

pub fn parse_rfc3339(value: &str) -> bool {
    DateTime::parse_from_rfc3339(value).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test/fixtures/ledger")
            .join(name)
    }

    fn body() -> WitnessBody {
        WitnessBody {
            task_uuid: Some("b2c40001-0000-4000-8000-000000000001".to_owned()),
            transition_timestamp: "2026-07-09T10:00:01.100Z".to_owned(),
            verdict: Verdict::Pass,
            exit_code: 0,
            artifact_content_hash: Some(format!("sha256:{}", "a".repeat(64))),
            gpu_seconds: Some(42.5),
            wall_clock: 44.0,
            attempt: 1,
            lease_epoch: 42,
            dedup_key: Some("ocr:paper-0001".to_owned()),
            payload_hash: None,
            labor_class: LaborClass::Fresh,
            trace_ref: None,
            pools: Some(vec!["worker-gpu".to_owned()]),
            executor: None,
            charge: Some(Charge {
                unit: "gpu-seconds".to_owned(),
                amount: 42.5,
                class_name: "verifiable".to_owned(),
            }),
            model: Some("vllm/qwen2-vl-ocr".to_owned()),
            evidence_class: None,
            manifest_hash: None,
            completion: None,
        }
    }

    #[test]
    fn ledger_fixture_is_green_and_tamper_is_red() {
        let valid = verify_file(&fixture("valid.jsonl")).unwrap();
        assert!(valid.ok, "{:?}", valid.problems);
        assert_eq!(valid.records, 4);

        let tampered = verify_file(&fixture("tampered.jsonl")).unwrap();
        assert!(!tampered.ok);
        assert!(tampered
            .problems
            .iter()
            .any(|problem| problem.kind == VerifyProblemKind::HashMismatch));
    }

    #[test]
    fn legacy_job_without_wave_three_features_keeps_identical_hash_input() {
        let record = build_record(body(), &ChainHead::default()).unwrap();
        assert!(record.completion.is_none());
        let raw = serde_json::to_value(&record).unwrap();
        assert_eq!(
            canonical_hash_input(&raw).unwrap(),
            concat!(
                "{\"task_uuid\":\"b2c40001-0000-4000-8000-000000000001\",",
                "\"transition_timestamp\":\"2026-07-09T10:00:01.100Z\",",
                "\"verdict\":\"pass\",\"exit_code\":0,",
                "\"artifact_content_hash\":\"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",",
                "\"gpu_seconds\":42.5,\"wall_clock\":44.0,\"attempt\":1,\"lease_epoch\":42,",
                "\"dedup_key\":\"ocr:paper-0001\",\"labor_class\":\"fresh\",",
                "\"pool\":\"worker-gpu\",",
                "\"charge\":{\"unit\":\"gpu-seconds\",\"amount\":42.5,\"class\":\"verifiable\"},",
                "\"model\":\"vllm/qwen2-vl-ocr\",\"seq\":1,",
                "\"prev_hash\":\"sha256:0000000000000000000000000000000000000000000000000000000000000000\",",
                "\"hash\":\"\"}"
            )
        );
        assert!(!serde_json::to_string(&record)
            .unwrap()
            .contains("\"completion\""));
        assert!(!serde_json::to_string(&record)
            .unwrap()
            .contains("\"payload_hash\""));
    }

    #[test]
    fn optional_metadata_is_ordered_before_seq_and_absent_stays_absent() {
        let absent = build_record(body(), &ChainHead::default()).unwrap();
        let absent_json = serde_json::to_string(&absent).unwrap();
        assert!(!absent_json.contains("evidence_class"));
        assert!(!absent_json.contains("manifest_hash"));
        assert!(!absent_json.contains("payload_hash"));

        let mut present_body = body();
        present_body.payload_hash = Some(format!("sha256:{}", "b".repeat(64)));
        present_body.evidence_class = Some(Value::String("opaque/class".to_owned()));
        present_body.manifest_hash = Some(Value::String("urn:manifest:anything".to_owned()));
        let present = build_record(present_body, &ChainHead::default()).unwrap();
        let json = serde_json::to_string(&present).unwrap();
        assert!(json.find("payload_hash").unwrap() < json.find("labor_class").unwrap());
        assert!(json.find("evidence_class").unwrap() < json.find("manifest_hash").unwrap());
        assert!(json.find("manifest_hash").unwrap() < json.find("\"seq\"").unwrap());
        let report = verify_reader(BufReader::new(json.as_bytes()));
        assert!(report.ok, "{:?}", report.problems);
    }

    #[test]
    fn witness_pool_encoding_preserves_legacy_bytes_and_canonicalizes_multi() {
        let singleton = build_record(body(), &ChainHead::default()).unwrap();
        let singleton_json = serde_json::to_string(&singleton).unwrap();
        assert!(singleton_json.contains(r#""pool":"worker-gpu""#));
        assert!(!singleton_json.contains(r#""pool":["#));

        let mut multi_body = body();
        multi_body.pools = Some(vec!["zeta".to_owned(), "alpha".to_owned()]);
        let multi = build_record(multi_body, &ChainHead::default()).unwrap();
        assert_eq!(
            multi.pools.as_deref(),
            Some(["alpha".to_owned(), "zeta".to_owned()].as_slice())
        );
        let multi_json = serde_json::to_string(&multi).unwrap();
        assert!(multi_json.contains(r#""pool":["alpha","zeta"]"#));
        let report = verify_reader(BufReader::new(multi_json.as_bytes()));
        assert!(report.ok, "{:?}", report.problems);
    }

    #[test]
    fn append_reopens_one_unbroken_fsynced_chain() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("witness.jsonl");
        {
            let mut ledger = WitnessLedger::open(&path).unwrap();
            assert_eq!(ledger.append(body()).unwrap().seq, 1);
        }
        {
            let mut ledger = WitnessLedger::open(&path).unwrap();
            assert_eq!(ledger.append(body()).unwrap().seq, 2);
        }
        assert!(verify_file(&path).unwrap().ok);
    }

    #[test]
    fn open_ledgers_lock_each_append_without_blocking_readers_or_each_other() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("witness.jsonl");
        let mut first = WitnessLedger::open(&path).unwrap();
        let mut second = WitnessLedger::open(&path).unwrap();

        assert_eq!(first.append(body()).unwrap().seq, 1);
        let (report, records) = read_verified_records(&path).unwrap();
        assert!(report.ok);
        assert_eq!(records.len(), 1);
        assert_eq!(second.append(body()).unwrap().seq, 2);
        assert_eq!(first.append(body()).unwrap().seq, 3);

        let (report, records) = read_verified_records(&path).unwrap();
        assert!(report.ok, "{:?}", report.problems);
        assert_eq!(
            records.iter().map(|record| record.seq).collect::<Vec<_>>(),
            [1, 2, 3]
        );
    }

    #[test]
    fn attestation_chain_is_independent_and_advisory() {
        assert_eq!(
            durable_parent(Path::new("attestations.jsonl")),
            Path::new(".")
        );
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("attestations.jsonl");
        append_attestation(&path, serde_json::json!({"capture": "session-1"})).unwrap();
        append_attestation(&path, serde_json::json!({"scraped_actual": 12})).unwrap();
        let report = verify_attestations(&path).unwrap();
        assert!(report.ok);
        assert_eq!(report.records, 2);
        assert_eq!(report.authentication, "unauthenticated-by-construction");

        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"observed_at\":")
            .unwrap();
        repair_attestation_tail(&path).unwrap();
        let report = verify_attestations(&path).unwrap();
        assert!(report.ok);
        assert_eq!(report.records, 2);
        assert_eq!(
            append_attestation(&path, serde_json::json!({"afterRepair": true}))
                .unwrap()
                .seq,
            3
        );

        let length = std::fs::metadata(&path).unwrap().len();
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(length - 1)
            .unwrap();
        repair_attestation_tail(&path).unwrap();
        assert!(std::fs::read(&path).unwrap().ends_with(b"\n"));
        assert_eq!(verify_attestations(&path).unwrap().records, 3);

        let malformed = temp.path().join("complete-but-invalid.jsonl");
        std::fs::write(&malformed, b"{\"payload\":{}}").unwrap();
        assert!(repair_attestation_tail(&malformed).is_err());
        assert_eq!(std::fs::read(&malformed).unwrap(), b"{\"payload\":{}}");
    }

    #[test]
    fn non_metered_verdicts_never_count() {
        for verdict in [
            Verdict::PoolVanished,
            Verdict::Preempted,
            Verdict::Cancelled,
        ] {
            let mut value = body();
            value.verdict = verdict;
            let record = build_record(value, &ChainHead::default()).unwrap();
            assert!(!counts_toward_canonical_gpu_seconds(&record));
        }
        let mut runtime_limited = body();
        runtime_limited.verdict = Verdict::RuntimeExceeded;
        let record = build_record(runtime_limited, &ChainHead::default()).unwrap();
        assert!(counts_toward_canonical_gpu_seconds(&record));
    }
}
