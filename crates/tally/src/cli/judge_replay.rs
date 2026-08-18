//! The judge-tier corpus replay harness (ETA.md §8.5, EPSILON-EXTENSION.md
//! ext2, AUGUST-01-DESIGN.md §2).
//!
//! Every tier decision about which model may hold the judge seat has been made
//! on instinct. The Aug-1 design named the empirical path instead — "every
//! diagnosis and narration is journaled, so a smaller candidate can be replayed
//! against the Sonnet corpus and the disagreement rate measured before any
//! swap" — and it has never been runnable. This is that procedure, deterministic
//! core first, in three parts:
//!
//! 1. [`assemble_corpus`] walks the durable record and emits a corpus of
//!    `{brief, recorded-verdict}` pairs. The record is two stores that were
//!    written for other reasons: the per-campaign attempt-receipt log carries
//!    the recorded diagnosis and its verdict, and the content-addressed brief
//!    archive carries the input each diagnosis was dispatched on. Neither one
//!    names the other, so the join is derived — see [`CaseKey`] — and it is
//!    lossy in ways that are facts about the record, not defects of the walk.
//!    The assembler therefore reports what it found *and* what it could not
//!    recover, with a reason per case.
//! 2. [`replay_corpus`] dispatches each retained brief to a candidate adapter
//!    resolved from the host catalog, exactly the way the production diagnosis
//!    node does: the workload is the brief sentinel, the brief arrives as the
//!    `TALLY_BRIEF` file (job units have no stdin — sitting C2), and the answer
//!    is read through the adapter's own `finalMessage` scrape.
//! 3. [`render_table`] renders the disagreement per case and in total, into a
//!    byte-stable table a seam sitting can commit as evidence.
//!
//! The harness is inert. It gates exactly one named decision and runs on no
//! timer, in no gate, and from no flow. In tests nothing calls a model: a
//! fixture candidate declared in a fixture catalog answers canned verdicts, and
//! that proves assembly, the replay plumbing, and the table. The live run
//! against a real candidate is a seam act performed by the operator side.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tally_core::adapters::AdapterEngine;
use tally_core::brief::BRIEF_DIRECTORY;

use super::text::compact_text;
use super::*;

/// Schema version of `corpus.json` and of the per-case recorded verdict.
const CORPUS_SCHEMA_VERSION: u64 = 1;
const CORPUS_MANIFEST: &str = "corpus.json";
const CASES_DIRECTORY: &str = "cases";
const BRIEF_FILE: &str = "brief.json";
const RECORDED_FILE: &str = "recorded.json";

/// Where a campaign's durable attempt receipts live under the state directory,
/// and the name of the log itself. Both are observed layout rather than a
/// constant the writer exports, so the flags exist for a record kept anywhere
/// else and these are only the defaults.
const RECEIPTS_SUBDIRECTORY: &str = "campaigns/attempt-receipts";
const RECEIPT_LOG_NAME: &str = "attempt-receipts-v1.jsonl";

/// Ceilings on the two durable inputs. Both stores are bounded by their own
/// writers (`brief::MAX_BRIEF_BYTES`, the driver's attempt-receipt log cap);
/// these are the reader's independent refusal to load an unbounded file it was
/// pointed at, because the assembler runs against a path an operator names.
const MAX_BRIEF_BYTES: u64 = 16 * 1024 * 1024;
const MAX_RECEIPT_LOG_BYTES: u64 = 64 * 1024 * 1024;

/// The upper bound the diagnosis result schema puts on `diagnosis`
/// (`examples/flows/spec-build.js`, `diagnosisResultSchema`). A candidate that
/// overruns it has failed the same schema the production node enforces.
const MAX_DIAGNOSIS_CHARS: usize = 12_000;

/// How long one candidate dispatch may take before the harness stops waiting.
/// A judge answering one recorded failure is a single short turn; the default
/// is generous for that and finite for a candidate that has wedged, because a
/// replay over a whole corpus must terminate without supervision.
pub(super) const DEFAULT_CANDIDATE_TIMEOUT_SEC: u64 = 600;

/// How much candidate-controlled text may reach a table cell.
const MAX_DETAIL_CHARS: usize = 160;

/// The typed verdict the diagnosis result schema admits and that deterministic
/// machinery executes. Replay compares exactly this, and nothing else: the
/// prose beside it is for a human, but the verdict is the decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Retry,
    Blocked,
    Transient,
}

impl Verdict {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "retry" => Some(Self::Retry),
            "blocked" => Some(Self::Blocked),
            "transient" => Some(Self::Transient),
            _ => None,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Retry => "retry",
            Self::Blocked => "blocked",
            Self::Transient => "transient",
        }
    }
}

/// The identity a corpus case is joined on.
///
/// Neither store carries the other's key: a receipt names no brief hash and a
/// brief names no receipt sequence. What both name is the campaign, the issue,
/// the task, and which attempt's failure was being judged — the receipt says so
/// directly, and the brief says so by the length of the `previousDiagnoses`
/// list it was rendered with. That tuple is therefore the join, and where it is
/// not unique on either side the record genuinely cannot say which brief
/// produced which verdict. That is reported, not guessed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CaseKey {
    campaign: String,
    issue_number: String,
    task_id: String,
    attempt: u64,
}

impl CaseKey {
    /// The corpus-directory name for this case.
    ///
    /// `+` separates because it appears in none of the three components'
    /// charsets — a campaign is a safe component, an issue number is digits, a
    /// task id is lowercase alphanumerics and dashes — so the rendering is
    /// injective and a case directory can never collide with another case.
    fn id(&self) -> String {
        format!(
            "{}+{}+{}+attempt-{}",
            self.campaign, self.issue_number, self.task_id, self.attempt
        )
    }
}

/// One retained diagnosis brief: the bytes a judge was dispatched on.
#[derive(Debug, Clone)]
struct RetainedBrief {
    path: PathBuf,
    bytes: Vec<u8>,
    sha256: String,
    /// Whether the content hash agrees with the content-addressed file name.
    /// A store entry whose name lies about its bytes is not a brief anybody can
    /// claim was dispatched, so it is unrecoverable rather than trusted.
    named_by_its_digest: bool,
}

/// One recorded diagnosis receipt: the verdict the seated judge returned.
#[derive(Debug, Clone)]
struct RecordedDiagnosis {
    verdict: Option<Verdict>,
    diagnosis: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CorpusManifest {
    schema_version: u64,
    campaigns: Vec<String>,
    found: usize,
    unrecoverable: usize,
    cases: Vec<CorpusCase>,
    unrecoverable_cases: Vec<UnrecoverableCase>,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CorpusCase {
    id: String,
    campaign: String,
    issue_number: String,
    task_id: String,
    attempt: u64,
    brief_sha256: String,
    recorded_verdict: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnrecoverableCase {
    campaign: String,
    issue_number: String,
    task_id: String,
    attempt: u64,
    /// Stable slug naming why this case cannot be replayed. Slugs, not
    /// sentences, because a seam sitting counts them.
    reason: String,
    detail: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecordedFile {
    schema_version: u64,
    campaign: String,
    issue_number: String,
    task_id: String,
    attempt: u64,
    verdict: String,
    diagnosis: String,
}

pub(super) async fn run_judge_replay(
    config_path: Option<&Path>,
    command: JudgeReplayCommand,
) -> Result<()> {
    match command {
        JudgeReplayCommand::Assemble(args) => assemble_corpus(args),
        JudgeReplayCommand::Run(args) => replay_corpus(config_path, args).await,
    }
}

// ---------------------------------------------------------------------------
// 1. Corpus assembly
// ---------------------------------------------------------------------------

fn assemble_corpus(args: JudgeReplayAssembleArgs) -> Result<()> {
    let campaigns: BTreeSet<String> = args.campaigns.iter().cloned().collect();
    for campaign in &campaigns {
        if !is_safe_component(campaign) {
            return Err(invalid(format!(
                "campaign {campaign:?} is not a safe path component"
            )));
        }
    }
    let briefs = match args.briefs {
        Some(path) => path,
        None => default_data_dir()?.join(BRIEF_DIRECTORY),
    };
    let receipts_root = match args.receipts_root {
        Some(path) => path,
        None => default_state_dir()?.join(RECEIPTS_SUBDIRECTORY),
    };

    let mut warnings = Vec::new();
    let retained = read_retained_briefs(&briefs, &campaigns, &mut warnings)?;
    let mut recorded: BTreeMap<CaseKey, Vec<RecordedDiagnosis>> = BTreeMap::new();
    for campaign in &campaigns {
        let log = receipts_root.join(campaign).join(RECEIPT_LOG_NAME);
        read_recorded_diagnoses(&log, campaign, &mut recorded, &mut warnings)?;
    }

    let mut cases = Vec::new();
    let mut unrecoverable = Vec::new();
    let keys: BTreeSet<&CaseKey> = retained.keys().chain(recorded.keys()).collect();
    for key in keys {
        let briefs_here = retained.get(key).map_or(&[][..], Vec::as_slice);
        let recorded_here = recorded.get(key).map_or(&[][..], Vec::as_slice);
        match classify_case(briefs_here, recorded_here) {
            Ok((brief, verdict)) => cases.push((key.clone(), brief.clone(), verdict.clone())),
            Err((reason, detail)) => unrecoverable.push(UnrecoverableCase {
                campaign: key.campaign.clone(),
                issue_number: key.issue_number.clone(),
                task_id: key.task_id.clone(),
                attempt: key.attempt,
                reason: reason.to_owned(),
                detail,
            }),
        }
    }

    let manifest = CorpusManifest {
        schema_version: CORPUS_SCHEMA_VERSION,
        campaigns: campaigns.iter().cloned().collect(),
        found: cases.len(),
        unrecoverable: unrecoverable.len(),
        cases: cases
            .iter()
            .map(|(key, brief, verdict)| CorpusCase {
                id: key.id(),
                campaign: key.campaign.clone(),
                issue_number: key.issue_number.clone(),
                task_id: key.task_id.clone(),
                attempt: key.attempt,
                brief_sha256: brief.sha256.clone(),
                recorded_verdict: verdict
                    .verdict
                    .expect("classified case has a verdict")
                    .as_str()
                    .to_owned(),
            })
            .collect(),
        unrecoverable_cases: unrecoverable,
        warnings,
    };

    write_corpus(&args.out, &manifest, &cases)?;

    if args.json {
        outln!("{}", serde_json::to_string_pretty(&manifest)?);
        return Ok(());
    }
    outln!(
        "corpus {} — {} replayable case(s), {} unrecoverable",
        args.out.display(),
        manifest.found,
        manifest.unrecoverable
    );
    for case in &manifest.cases {
        outln!(
            "  found         {} recorded={}",
            case.id,
            case.recorded_verdict
        );
    }
    for case in &manifest.unrecoverable_cases {
        outln!(
            "  unrecoverable {}+{}+{}+attempt-{} {} ({})",
            case.campaign,
            case.issue_number,
            case.task_id,
            case.attempt,
            case.reason,
            case.detail
        );
    }
    for warning in &manifest.warnings {
        errln!("warning: {warning}");
    }
    Ok(())
}

/// Decide whether one join key yields a replayable case, or name why not.
///
/// Every rejection here is a statement about the durable record: a supervisor
/// who diagnosed by hand left a receipt and no brief; a dispatch that died
/// before its verdict was written left a brief and no receipt; the legacy
/// receipt schema carried no `verdict` field at all, so a whole campaign's
/// diagnoses can be present and still say nothing this harness can measure
/// against.
fn classify_case<'a>(
    briefs: &'a [RetainedBrief],
    recorded: &'a [RecordedDiagnosis],
) -> std::result::Result<(&'a RetainedBrief, &'a RecordedDiagnosis), (&'static str, String)> {
    if recorded.is_empty() {
        return Err((
            "verdict-not-recorded",
            "a diagnosis brief was retained but no receipt records a verdict for it".to_owned(),
        ));
    }
    if briefs.is_empty() {
        return Err((
            "brief-not-retained",
            "a verdict is recorded but no diagnosis brief for it survives in the archive"
                .to_owned(),
        ));
    }
    if briefs.len() > 1 {
        return Err((
            "ambiguous-brief",
            format!(
                "{} retained briefs carry this task and attempt; the record cannot say which one produced the recorded verdict",
                briefs.len()
            ),
        ));
    }
    let brief = &briefs[0];
    if !brief.named_by_its_digest {
        return Err((
            "brief-digest-mismatch",
            format!(
                "content-addressed brief {} does not hash to its own name",
                brief.path.display()
            ),
        ));
    }
    let verdicts: BTreeSet<Option<&'static str>> = recorded
        .iter()
        .map(|entry| entry.verdict.map(Verdict::as_str))
        .collect();
    if verdicts.len() > 1 {
        return Err((
            "ambiguous-recorded-verdict",
            format!(
                "{} receipts carry this task and attempt and disagree on the verdict",
                recorded.len()
            ),
        ));
    }
    let entry = recorded
        .iter()
        .find(|entry| entry.verdict.is_some())
        .unwrap_or(&recorded[0]);
    if entry.verdict.is_none() {
        return Err((
            "recorded-verdict-absent",
            "the receipt predates the typed verdict field, so it records prose and no decision"
                .to_owned(),
        ));
    }
    Ok((brief, entry))
}

/// Walk the content-addressed brief archive for diagnosis briefs of the named
/// campaigns.
///
/// The archive is flat and holds every role's briefs, so the filter is the
/// document's own `role` field — the one the role-aware steward shim branches
/// on. `previousDiagnoses` is the attempt index by construction: the flow
/// renders attempt `previousDiagnoses.length + 1`.
fn read_retained_briefs(
    directory: &Path,
    campaigns: &BTreeSet<String>,
    warnings: &mut Vec<String>,
) -> Result<BTreeMap<CaseKey, Vec<RetainedBrief>>> {
    let mut retained: BTreeMap<CaseKey, Vec<RetainedBrief>> = BTreeMap::new();
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            warnings.push(format!(
                "brief archive {} does not exist; every recorded verdict is unrecoverable",
                directory.display()
            ));
            return Ok(retained);
        }
        Err(error) => {
            return Err(anyhow::Error::new(error)
                .context(format!("cannot read brief archive {}", directory.display())))
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| {
            format!(
                "cannot read brief archive entry under {}",
                directory.display()
            )
        })?;
        let path = entry.path();
        if path.extension().and_then(OsStr::to_str) == Some("json") {
            paths.push(path);
        }
    }
    // A directory walk has no order; the corpus must not depend on one.
    paths.sort();
    for path in paths {
        let metadata = std::fs::symlink_metadata(&path)
            .with_context(|| format!("cannot inspect brief {}", path.display()))?;
        if !metadata.is_file() {
            continue;
        }
        if metadata.len() > MAX_BRIEF_BYTES {
            warnings.push(format!(
                "brief {} exceeds {MAX_BRIEF_BYTES} bytes and was not read",
                path.display()
            ));
            continue;
        }
        let bytes = std::fs::read(&path)
            .with_context(|| format!("cannot read brief {}", path.display()))?;
        let Ok(document) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        if document.get("role").and_then(Value::as_str) != Some("diagnosis") {
            continue;
        }
        let Some(key) = diagnosis_brief_key(&document) else {
            warnings.push(format!(
                "diagnosis brief {} names no campaign, task, and attempt this walk can key on",
                path.display()
            ));
            continue;
        };
        if !campaigns.contains(&key.campaign) {
            continue;
        }
        let sha256 = format!("sha256:{:x}", Sha256::digest(&bytes));
        let named_by_its_digest = path
            .file_stem()
            .and_then(OsStr::to_str)
            .is_some_and(|stem| sha256 == format!("sha256:{stem}"));
        retained.entry(key).or_default().push(RetainedBrief {
            path,
            bytes,
            sha256,
            named_by_its_digest,
        });
    }
    Ok(retained)
}

/// The join key a diagnosis brief carries, or `None` if it carries no usable
/// one. Unsafe components are rejected rather than sanitized: a case directory
/// is named from these bytes.
fn diagnosis_brief_key(document: &Value) -> Option<CaseKey> {
    let campaign = document.get("campaign")?;
    let name = campaign.get("name").and_then(Value::as_str)?;
    let issue_number = campaign
        .get("issue")?
        .get("number")
        .and_then(Value::as_str)?;
    let task_id = document.get("task")?.get("id").and_then(Value::as_str)?;
    let attempt = document.get("previousDiagnoses")?.as_array()?.len() as u64 + 1;
    if !is_safe_component(name) || !is_issue_number(issue_number) || !is_safe_task_id(task_id) {
        return None;
    }
    Some(CaseKey {
        campaign: name.to_owned(),
        issue_number: issue_number.to_owned(),
        task_id: task_id.to_owned(),
        attempt,
    })
}

/// Read one campaign's durable attempt-receipt log for its diagnosis records.
///
/// The log is append-only JSON lines written by the driver across every arm of
/// a campaign, so it is read leniently: a line this walk cannot parse is a
/// warning, never a refusal, because refusing would make one malformed tail
/// hide every sound record ahead of it.
fn read_recorded_diagnoses(
    path: &Path,
    campaign: &str,
    recorded: &mut BTreeMap<CaseKey, Vec<RecordedDiagnosis>>,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            warnings.push(format!(
                "campaign {campaign:?} has no attempt-receipt log at {}; it contributes no recorded verdicts",
                path.display()
            ));
            return Ok(());
        }
        Err(error) => {
            return Err(anyhow::Error::new(error).context(format!(
                "cannot inspect attempt-receipt log {}",
                path.display()
            )))
        }
    };
    if !metadata.is_file() || metadata.len() > MAX_RECEIPT_LOG_BYTES {
        return Err(invalid(format!(
            "attempt-receipt log {} is not a bounded regular file",
            path.display()
        )));
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read attempt-receipt log {}", path.display()))?;
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            warnings.push(format!(
                "attempt-receipt log {} line {} is not JSON",
                path.display(),
                index + 1
            ));
            continue;
        };
        if record.get("kind").and_then(Value::as_str) != Some("diagnosis") {
            continue;
        }
        if record.get("campaign").and_then(Value::as_str) != Some(campaign) {
            continue;
        }
        let (Some(issue_number), Some(task_id), Some(attempt)) = (
            record.get("issueNumber").and_then(Value::as_str),
            record.get("taskId").and_then(Value::as_str),
            record.get("attempt").and_then(Value::as_u64),
        ) else {
            warnings.push(format!(
                "attempt-receipt log {} line {} is a diagnosis with no task identity",
                path.display(),
                index + 1
            ));
            continue;
        };
        if !is_issue_number(issue_number) || !is_safe_task_id(task_id) {
            warnings.push(format!(
                "attempt-receipt log {} line {} names an unsafe task identity",
                path.display(),
                index + 1
            ));
            continue;
        }
        // A `verdict` this walk does not recognise is not silently dropped to
        // "absent": absent means the record predates the field, and a foreign
        // spelling means something else wrote it. Both are unrecoverable, but
        // only one of them is the legacy schema.
        let verdict = match record.get("verdict") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) => match Verdict::parse(value) {
                Some(verdict) => Some(verdict),
                None => {
                    warnings.push(format!(
                        "attempt-receipt log {} line {} records an undeclared verdict",
                        path.display(),
                        index + 1
                    ));
                    continue;
                }
            },
            Some(_) => {
                warnings.push(format!(
                    "attempt-receipt log {} line {} records a non-string verdict",
                    path.display(),
                    index + 1
                ));
                continue;
            }
        };
        let key = CaseKey {
            campaign: campaign.to_owned(),
            issue_number: issue_number.to_owned(),
            task_id: task_id.to_owned(),
            attempt,
        };
        recorded.entry(key).or_default().push(RecordedDiagnosis {
            verdict,
            diagnosis: record
                .get("diagnosis")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        });
    }
    Ok(())
}

fn write_corpus(
    out: &Path,
    manifest: &CorpusManifest,
    cases: &[(CaseKey, RetainedBrief, RecordedDiagnosis)],
) -> Result<()> {
    match std::fs::read_dir(out) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                return Err(invalid(format!(
                    "corpus directory {} already holds entries; assemble into a fresh directory",
                    out.display()
                )));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(out)
                .with_context(|| format!("cannot create corpus directory {}", out.display()))?;
        }
        Err(error) => {
            return Err(anyhow::Error::new(error)
                .context(format!("cannot inspect corpus directory {}", out.display())))
        }
    }
    for (key, brief, recorded) in cases {
        let directory = out.join(CASES_DIRECTORY).join(key.id());
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("cannot create case directory {}", directory.display()))?;
        // Verbatim bytes. The brief is a content-addressed document and the
        // corpus records its hash; a re-serialized copy would be a different
        // document that no receipt was ever written against.
        std::fs::write(directory.join(BRIEF_FILE), &brief.bytes)
            .with_context(|| format!("cannot write brief into {}", directory.display()))?;
        let recorded = RecordedFile {
            schema_version: CORPUS_SCHEMA_VERSION,
            campaign: key.campaign.clone(),
            issue_number: key.issue_number.clone(),
            task_id: key.task_id.clone(),
            attempt: key.attempt,
            verdict: recorded
                .verdict
                .expect("classified case has a verdict")
                .as_str()
                .to_owned(),
            diagnosis: recorded.diagnosis.clone(),
        };
        std::fs::write(
            directory.join(RECORDED_FILE),
            format!("{}\n", serde_json::to_string_pretty(&recorded)?),
        )
        .with_context(|| format!("cannot write recorded verdict into {}", directory.display()))?;
    }
    std::fs::write(
        out.join(CORPUS_MANIFEST),
        format!("{}\n", serde_json::to_string_pretty(manifest)?),
    )
    .with_context(|| format!("cannot write corpus manifest into {}", out.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 2. Replay
// ---------------------------------------------------------------------------

/// What one case's replay established. Exactly the three the design names:
/// the candidate agreed, the candidate returned a different verdict class, or
/// the candidate never produced a verdict the schema admits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Match,
    VerdictClassMismatch,
    SchemaFailure,
}

impl Outcome {
    const fn label(self) -> &'static str {
        match self {
            Self::Match => "match",
            Self::VerdictClassMismatch => "verdict-class-mismatch",
            Self::SchemaFailure => "schema-failure",
        }
    }

    /// Whether this outcome counts against the candidate. A schema failure is
    /// a disagreement with the seat, not a neutral skip: a judge that cannot
    /// answer the contract cannot hold the seat.
    const fn disagrees(self) -> bool {
        !matches!(self, Self::Match)
    }
}

/// The candidate's answer, already reduced to what the table can render.
#[derive(Debug, Clone)]
enum CandidateAnswer {
    Verdict(Verdict),
    /// Why no verdict was obtained. Candidate-controlled text reaches this,
    /// so it is sanitized and bounded at construction — the table is committed
    /// as evidence and read in a terminal.
    Failure(String),
}

impl CandidateAnswer {
    fn failure(detail: impl AsRef<str>) -> Self {
        Self::Failure(bounded_detail(detail.as_ref()))
    }
}

#[derive(Debug, Clone)]
struct ReplayedCase {
    id: String,
    recorded: Verdict,
    answer: CandidateAnswer,
    outcome: Outcome,
}

async fn replay_corpus(config_path: Option<&Path>, args: JudgeReplayRunArgs) -> Result<()> {
    if args.timeout_sec == 0 {
        return Err(invalid("--timeout-sec must be positive"));
    }
    let manifest = read_corpus_manifest(&args.corpus)?;
    let config = load_client_config(config_path)?;
    let adapter = config.adapters.get(&args.candidate).ok_or_else(|| {
        invalid(format!(
            "unknown candidate adapter {:?}; configured adapters: {}",
            args.candidate,
            configured_names(config.adapters.keys())
        ))
    })?;
    // An adapter with no argv of its own — `shell` is the shipped example —
    // would take the brief sentinel as its executable. A judge candidate names
    // its own binary or it is not a candidate.
    if adapter.argv.is_empty() {
        return Err(invalid(format!(
            "candidate adapter {:?} declares no argv; a judge candidate must name its own executable",
            args.candidate
        )));
    }
    let engine = AdapterEngine::new(&config.adapters);
    let timeout = Duration::from_secs(args.timeout_sec);

    let mut replayed = Vec::with_capacity(manifest.cases.len());
    for case in &manifest.cases {
        let recorded = Verdict::parse(&case.recorded_verdict).ok_or_else(|| {
            invalid(format!(
                "corpus case {} records the undeclared verdict {:?}",
                case.id, case.recorded_verdict
            ))
        })?;
        let directory = args.corpus.join(CASES_DIRECTORY).join(&case.id);
        let brief = directory.join(BRIEF_FILE);
        let bytes = std::fs::read(&brief)
            .with_context(|| format!("cannot read corpus brief {}", brief.display()))?;
        let sha256 = format!("sha256:{:x}", Sha256::digest(&bytes));
        if sha256 != case.brief_sha256 {
            return Err(invalid(format!(
                "corpus brief {} hashes to {sha256}, but the manifest records {}",
                brief.display(),
                case.brief_sha256
            )));
        }
        let answer = dispatch_candidate(
            &engine,
            &args.candidate,
            &directory,
            &brief,
            &sha256,
            timeout,
        )
        .await?;
        let outcome = match &answer {
            CandidateAnswer::Verdict(verdict) if *verdict == recorded => Outcome::Match,
            CandidateAnswer::Verdict(_) => Outcome::VerdictClassMismatch,
            CandidateAnswer::Failure(_) => Outcome::SchemaFailure,
        };
        replayed.push(ReplayedCase {
            id: case.id.clone(),
            recorded,
            answer,
            outcome,
        });
    }

    let table = render_table(&manifest, &args.candidate, &replayed);
    std::fs::write(&args.out, &table)
        .with_context(|| format!("cannot write disagreement table {}", args.out.display()))?;
    for line in table.lines() {
        outln!("{line}");
    }
    Ok(())
}

fn read_corpus_manifest(corpus: &Path) -> Result<CorpusManifest> {
    let path = corpus.join(CORPUS_MANIFEST);
    let bytes = std::fs::read(&path)
        .with_context(|| format!("cannot read corpus manifest {}", path.display()))?;
    let manifest: CorpusManifest = serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "corpus manifest {} is not a corpus manifest",
            path.display()
        )
    })?;
    if manifest.schema_version != CORPUS_SCHEMA_VERSION {
        return Err(invalid(format!(
            "corpus manifest {} declares schema version {}, not {CORPUS_SCHEMA_VERSION}",
            path.display(),
            manifest.schema_version
        )));
    }
    Ok(manifest)
}

/// Dispatch one recorded brief to the candidate the way production does.
///
/// The workload argv is the brief sentinel and the document arrives as the
/// `TALLY_BRIEF` file, because that is the shape a diagnosis dispatch has: a
/// diagnosis is a daemon job unit, job units have no stdin, and a steward that
/// reads stdin answers every diagnosis from an empty read (sitting C2). The
/// answer is read through the adapter's own declared `finalMessage` scrape
/// rather than off raw stdout, so a candidate is replayed under the same
/// contract the seat enforces and not under one this harness invented.
///
/// Every way a candidate can fail to answer is the candidate's outcome, not the
/// harness's error: an exit code, a stream that will not scrape, an answer that
/// is not the diagnosis result schema, and a candidate that never returns all
/// end in [`CandidateAnswer::Failure`]. Only a failure to *launch* is this
/// function's own error, because that is a fact about the catalog.
async fn dispatch_candidate(
    engine: &AdapterEngine<'_>,
    candidate: &str,
    case_directory: &Path,
    brief: &Path,
    brief_sha256: &str,
    timeout: Duration,
) -> Result<CandidateAnswer> {
    let invocation = engine
        .launch(
            candidate,
            &[tally_core::campaign_contract::BRIEF_SENTINEL.to_owned()],
        )
        .map_err(|error| {
            invalid(format!(
                "candidate adapter {candidate:?} cannot render a diagnosis launch: {error}"
            ))
        })?;
    let (program, rest) = invocation
        .argv
        .split_first()
        .ok_or_else(|| invalid(format!("candidate adapter {candidate:?} rendered no argv")))?;
    let mut command = tokio::process::Command::new(program);
    command
        .args(rest)
        .current_dir(case_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (name, value) in &invocation.env {
        command.env(name, value);
    }
    command
        .env("TALLY_BRIEF", brief)
        .env("TALLY_BRIEF_HASH", brief_sha256);
    let child = command.spawn().map_err(|error| {
        invalid(format!(
            "cannot launch candidate adapter {candidate:?} ({program}): {error}"
        ))
    })?;
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(output) => output.with_context(|| {
            format!("cannot collect the output of candidate adapter {candidate:?}")
        })?,
        // `kill_on_drop` reaps the child as the timed-out future is dropped.
        Err(_) => {
            return Ok(CandidateAnswer::failure(format!(
                "the candidate did not answer within {} s",
                timeout.as_secs()
            )))
        }
    };
    if !output.status.success() {
        return Ok(CandidateAnswer::failure(match output.status.code() {
            Some(code) => format!("the candidate exited {code}"),
            None => "the candidate was terminated by a signal".to_owned(),
        }));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let scraped = match engine.scrape_text(candidate, &stdout, &stderr) {
        Ok(scraped) => scraped,
        Err(error) => {
            return Ok(CandidateAnswer::failure(format!(
                "the candidate stream did not scrape: {error}"
            )))
        }
    };
    let message = match scraped.final_message() {
        Ok(Some(message)) => message.to_owned(),
        Ok(None) => {
            return Ok(CandidateAnswer::failure(
                "the candidate stream carried no finalMessage capture",
            ))
        }
        Err(error) => {
            return Ok(CandidateAnswer::failure(format!(
                "the candidate finalMessage capture is not a string: {error}"
            )))
        }
    };
    Ok(parse_diagnosis_result(&message))
}

/// Validate one candidate answer against the diagnosis result schema the
/// production node forces (`examples/flows/spec-build.js`,
/// `diagnosisResultSchema`): `{verdict, diagnosis}` required, `proposal`
/// permitted only beside a blocked verdict, no other properties.
fn parse_diagnosis_result(message: &str) -> CandidateAnswer {
    let Ok(value) = serde_json::from_str::<Value>(message.trim()) else {
        return CandidateAnswer::failure("the answer is not JSON");
    };
    let Some(object) = value.as_object() else {
        return CandidateAnswer::failure("the answer is not a JSON object");
    };
    for key in object.keys() {
        if !matches!(key.as_str(), "verdict" | "diagnosis" | "proposal") {
            return CandidateAnswer::failure(format!(
                "the answer carries the undeclared field {key:?}"
            ));
        }
    }
    let Some(verdict) = object.get("verdict").and_then(Value::as_str) else {
        return CandidateAnswer::failure("the answer names no verdict string");
    };
    let Some(verdict) = Verdict::parse(verdict) else {
        return CandidateAnswer::failure(format!("{verdict:?} is not a declared verdict"));
    };
    let Some(diagnosis) = object.get("diagnosis").and_then(Value::as_str) else {
        return CandidateAnswer::failure("the answer names no diagnosis string");
    };
    let length = diagnosis.chars().count();
    if length == 0 || length > MAX_DIAGNOSIS_CHARS {
        return CandidateAnswer::failure(format!(
            "the answer's diagnosis is {length} characters, outside 1..={MAX_DIAGNOSIS_CHARS}"
        ));
    }
    if object.contains_key("proposal") && verdict != Verdict::Blocked {
        return CandidateAnswer::failure("the answer rides a proposal on a non-blocked verdict");
    }
    CandidateAnswer::Verdict(verdict)
}

// ---------------------------------------------------------------------------
// 3. The disagreement table
// ---------------------------------------------------------------------------

/// Render the disagreement table.
///
/// Byte-stable by construction: the rows are the manifest's own order, the
/// column widths derive from the content, the rate is integer arithmetic rather
/// than a formatted float, and nothing here reads a clock, a duration, a
/// process id, or a path outside the corpus. Two replays of the same corpus
/// against the same deterministic candidate produce identical bytes, which is
/// what makes the file worth committing as evidence.
fn render_table(manifest: &CorpusManifest, candidate: &str, cases: &[ReplayedCase]) -> String {
    let headers = ["case", "recorded", "candidate", "outcome"];
    let rows: Vec<[String; 4]> = cases
        .iter()
        .map(|case| {
            [
                case.id.clone(),
                case.recorded.as_str().to_owned(),
                match &case.answer {
                    CandidateAnswer::Verdict(verdict) => verdict.as_str().to_owned(),
                    CandidateAnswer::Failure(_) => "-".to_owned(),
                },
                case.outcome.label().to_owned(),
            ]
        })
        .collect();
    let widths: Vec<usize> = (0..headers.len())
        .map(|column| {
            rows.iter()
                .map(|row| row[column].chars().count())
                .chain(std::iter::once(headers[column].chars().count()))
                .max()
                .unwrap_or_default()
        })
        .collect();

    let mut out = String::new();
    out.push_str("# judge-tier corpus replay — disagreement table\n");
    out.push_str(&format!("schemaVersion: {CORPUS_SCHEMA_VERSION}\n"));
    out.push_str(&format!("candidate: {candidate}\n"));
    out.push_str(&format!("campaigns: {}\n", manifest.campaigns.join(" ")));
    out.push_str(&format!(
        "corpus: {} replayable case(s), {} unrecoverable\n\n",
        manifest.found, manifest.unrecoverable
    ));
    out.push_str(&format!(
        "{}\n",
        render_row(&headers.map(String::from), &widths)
    ));
    let rule: Vec<String> = widths.iter().map(|width| "-".repeat(*width)).collect();
    out.push_str(&format!(
        "{}\n",
        render_row(
            &[
                rule[0].clone(),
                rule[1].clone(),
                rule[2].clone(),
                rule[3].clone()
            ],
            &widths
        )
    ));
    for row in &rows {
        out.push_str(&format!("{}\n", render_row(row, &widths)));
    }

    let total = cases.len();
    let matched = cases
        .iter()
        .filter(|case| case.outcome == Outcome::Match)
        .count();
    let mismatched = cases
        .iter()
        .filter(|case| case.outcome == Outcome::VerdictClassMismatch)
        .count();
    let failed = cases
        .iter()
        .filter(|case| case.outcome == Outcome::SchemaFailure)
        .count();
    let disagreements = cases.iter().filter(|case| case.outcome.disagrees()).count();
    out.push_str(&format!(
        "\ntotals: cases {total}  match {matched}  verdict-class-mismatch {mismatched}  schema-failure {failed}\n"
    ));
    out.push_str(&format!(
        "disagreement: {disagreements}/{total} = {}\n",
        rate(disagreements, total)
    ));

    if failed > 0 {
        out.push_str("\nschema failures\n");
        for case in cases {
            if let CandidateAnswer::Failure(detail) = &case.answer {
                out.push_str(&format!("- {}: {detail}\n", case.id));
            }
        }
    }
    out
}

fn render_row(cells: &[String; 4], widths: &[usize]) -> String {
    let mut line = String::new();
    for (index, cell) in cells.iter().enumerate() {
        if index > 0 {
            line.push_str("  ");
        }
        line.push_str(cell);
        if index + 1 < cells.len() {
            let padding = widths[index].saturating_sub(cell.chars().count());
            line.push_str(&" ".repeat(padding));
        }
    }
    line
}

/// The disagreement rate to two decimals, computed in integers.
///
/// A float formatted at two decimals would be byte-stable in practice, but the
/// table's whole claim is that identical inputs give identical bytes; integer
/// arithmetic makes that true by construction rather than by trusting a
/// formatter's rounding across versions.
fn rate(numerator: usize, denominator: usize) -> String {
    if denominator == 0 {
        return "n/a (empty corpus)".to_owned();
    }
    let hundredths = (numerator * 10_000 + denominator / 2) / denominator;
    format!("{}.{:02}%", hundredths / 100, hundredths % 100)
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Bound and de-fang one piece of candidate-controlled text for a table cell.
fn bounded_detail(value: &str) -> String {
    let compact = compact_text(value);
    if compact.chars().count() <= MAX_DETAIL_CHARS {
        return compact;
    }
    let head: String = compact.chars().take(MAX_DETAIL_CHARS).collect();
    format!("{head}…")
}

/// A campaign name safe as one path component: the driver's own `is_component`
/// charset, minus the leading dot that would make a hidden or relative name.
fn is_safe_component(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && (bytes[0].is_ascii_alphanumeric() || bytes[0] == b'_')
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'.' | b'-'))
        && !value.contains('+')
}

/// The driver's `is_task_id` charset: lowercase alphanumerics and dashes, with
/// no leading or trailing dash.
fn is_safe_task_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    let alphanumeric = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    !bytes.is_empty()
        && alphanumeric(bytes[0])
        && alphanumeric(bytes[bytes.len() - 1])
        && bytes
            .iter()
            .all(|byte| alphanumeric(*byte) || *byte == b'-')
}

fn is_issue_number(value: &str) -> bool {
    !value.is_empty() && value.len() <= 20 && value.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three outcome classes the design names, and nothing else: a table a
    /// seam sitting reads must not grow a fourth column of judgment.
    #[test]
    fn replay_classifies_a_candidate_answer_against_the_diagnosis_schema() {
        assert!(matches!(
            parse_diagnosis_result(r#"{"verdict":"retry","diagnosis":"a"}"#),
            CandidateAnswer::Verdict(Verdict::Retry)
        ));
        assert!(matches!(
            parse_diagnosis_result(r#"{"verdict":"blocked","diagnosis":"a","proposal":{}}"#),
            CandidateAnswer::Verdict(Verdict::Blocked)
        ));
        for off_schema in [
            "not json",
            "[]",
            r#"{"verdict":"retry"}"#,
            r#"{"diagnosis":"a"}"#,
            r#"{"verdict":"maybe","diagnosis":"a"}"#,
            r#"{"verdict":"retry","diagnosis":""}"#,
            r#"{"verdict":"retry","diagnosis":"a","proposal":{}}"#,
            r#"{"verdict":"retry","diagnosis":"a","extra":1}"#,
        ] {
            assert!(
                matches!(
                    parse_diagnosis_result(off_schema),
                    CandidateAnswer::Failure(_)
                ),
                "{off_schema} was admitted"
            );
        }
    }

    /// The rate is what the §8.5 decision is taken on, so it rounds by integer
    /// arithmetic that cannot drift with a formatter.
    #[test]
    fn replay_renders_the_disagreement_rate_without_a_float() {
        assert_eq!(rate(0, 0), "n/a (empty corpus)");
        assert_eq!(rate(0, 4), "0.00%");
        assert_eq!(rate(4, 4), "100.00%");
        assert_eq!(rate(2, 3), "66.67%");
        assert_eq!(rate(1, 3), "33.33%");
    }

    /// A case directory is named from record bytes, so the components that
    /// reach it are checked rather than trusted.
    #[test]
    fn replay_refuses_record_identities_that_are_not_safe_case_components() {
        assert!(is_safe_component("epsilon-extension"));
        assert!(is_safe_component("eta"));
        assert!(!is_safe_component(""));
        assert!(!is_safe_component("../escape"));
        assert!(!is_safe_component(".hidden"));
        assert!(!is_safe_component("with+plus"));
        assert!(is_safe_task_id("baseline-parity-probe"));
        assert!(!is_safe_task_id("-leading"));
        assert!(!is_safe_task_id("Upper"));
        assert!(!is_safe_task_id("with/slash"));
        assert!(is_issue_number("1"));
        assert!(!is_issue_number(""));
        assert!(!is_issue_number("1a"));
    }

    /// Every unrecoverable class the durable record actually produces, named
    /// from the record rather than guessed around.
    #[test]
    fn replay_assembly_names_why_a_case_is_unrecoverable() {
        let brief = |named_by_its_digest| RetainedBrief {
            path: PathBuf::from("/briefs/a.json"),
            bytes: Vec::new(),
            sha256: "sha256:a".to_owned(),
            named_by_its_digest,
        };
        let recorded = |verdict| RecordedDiagnosis {
            verdict,
            diagnosis: "prose".to_owned(),
        };
        let reason = |briefs: &[RetainedBrief], records: &[RecordedDiagnosis]| {
            classify_case(briefs, records)
                .err()
                .map(|(reason, _)| reason)
        };
        assert_eq!(reason(&[brief(true)], &[]), Some("verdict-not-recorded"));
        assert_eq!(
            reason(&[], &[recorded(Some(Verdict::Retry))]),
            Some("brief-not-retained")
        );
        assert_eq!(
            reason(
                &[brief(true), brief(true)],
                &[recorded(Some(Verdict::Retry))]
            ),
            Some("ambiguous-brief")
        );
        assert_eq!(
            reason(&[brief(false)], &[recorded(Some(Verdict::Retry))]),
            Some("brief-digest-mismatch")
        );
        assert_eq!(
            reason(&[brief(true)], &[recorded(None)]),
            Some("recorded-verdict-absent")
        );
        assert_eq!(
            reason(
                &[brief(true)],
                &[
                    recorded(Some(Verdict::Retry)),
                    recorded(Some(Verdict::Blocked))
                ]
            ),
            Some("ambiguous-recorded-verdict")
        );
        assert!(classify_case(&[brief(true)], &[recorded(Some(Verdict::Retry))]).is_ok());
        // Two receipts that agree are one recorded decision, not an ambiguity.
        assert!(classify_case(
            &[brief(true)],
            &[
                recorded(Some(Verdict::Retry)),
                recorded(Some(Verdict::Retry))
            ]
        )
        .is_ok());
    }
}
