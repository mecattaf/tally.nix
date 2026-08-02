use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::witness::{
    read_verified_records, AuthorshipStatus, VerifyReport, WitnessError, WitnessRecord,
};

// 2 adds the revision mode: `ledgerPath`, `ledger`, and `taskUuid` are
// omitted when the verifier was pointed at a bare revision instead of at a
// witnessed task lane.
pub const AUTHORSHIP_VERIFY_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthorshipVerificationStatus {
    Match,
    LedgerInvalid,
    WitnessNotFound,
    NotBound,
    RevisionMissing,
    MissingNote,
    NoteContentMismatch,
    NotesRefTargetMismatch,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AuthorshipVerificationReport {
    pub schema_version: u32,
    pub ok: bool,
    pub status: AuthorshipVerificationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger_path: Option<PathBuf>,
    pub repository: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger: Option<VerifyReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lease_epoch: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub witness_seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorship_status: Option<AuthorshipStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_notes_ref_target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_notes_ref_target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_note_content_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_note_content_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

fn blank_report(repository: &Path) -> AuthorshipVerificationReport {
    AuthorshipVerificationReport {
        schema_version: AUTHORSHIP_VERIFY_SCHEMA_VERSION,
        ok: false,
        status: AuthorshipVerificationStatus::Error,
        ledger_path: None,
        repository: repository.to_owned(),
        ledger: None,
        task_uuid: None,
        attempt: None,
        lease_epoch: None,
        witness_seq: None,
        result_revision: None,
        authorship_status: None,
        provider: None,
        provider_version: None,
        note_ref: None,
        expected_notes_ref_target: None,
        observed_notes_ref_target: None,
        expected_note_content_sha256: None,
        observed_note_content_sha256: None,
        reason: None,
    }
}

pub fn verify_authorship(
    ledger_path: &Path,
    repository: &Path,
    task_uuid: &str,
    attempt: Option<u32>,
    lease_epoch: Option<u64>,
) -> Result<AuthorshipVerificationReport, WitnessError> {
    let (ledger, records) = read_verified_records(ledger_path)?;
    let mut report = AuthorshipVerificationReport {
        status: AuthorshipVerificationStatus::WitnessNotFound,
        ledger_path: Some(ledger_path.to_owned()),
        ledger: Some(ledger),
        task_uuid: Some(task_uuid.to_owned()),
        attempt,
        lease_epoch,
        ..blank_report(repository)
    };
    if !report.ledger.as_ref().is_some_and(|ledger| ledger.ok) {
        report.status = AuthorshipVerificationStatus::LedgerInvalid;
        report.reason = Some(
            "the verdict witness chain is invalid; repository provenance was not evaluated"
                .to_owned(),
        );
        return Ok(report);
    }

    let selected = select_witness(&records, task_uuid, attempt, lease_epoch);
    let Some(witness) = selected else {
        report.reason = Some("no witness record matches the requested task lane".to_owned());
        return Ok(report);
    };
    report.attempt = Some(witness.attempt);
    report.lease_epoch = Some(witness.lease_epoch);
    report.witness_seq = Some(witness.seq);
    report.result_revision = witness.result_revision.clone();
    let Some(authorship) = witness.authorship.as_ref() else {
        report.status = AuthorshipVerificationStatus::NotBound;
        report.reason = Some("the selected witness has no authorship binding".to_owned());
        return Ok(report);
    };
    report.authorship_status = Some(authorship.status);
    report.provider = Some(authorship.provider.clone());
    report.provider_version = Some(authorship.provider_version.clone());
    report.note_ref = Some(authorship.note_ref.clone());
    report.expected_notes_ref_target = authorship.notes_ref_target.clone();
    report.expected_note_content_sha256 = authorship.note_content_sha256.clone();

    let Some(revision) = witness.result_revision.clone() else {
        report.status = AuthorshipVerificationStatus::NotBound;
        report.reason = Some("the selected witness has no result revision".to_owned());
        return Ok(report);
    };
    let Some(expected_target) = authorship.notes_ref_target.clone() else {
        report.status = AuthorshipVerificationStatus::NotBound;
        report.reason = authorship
            .reason
            .clone()
            .or_else(|| Some("the selected witness has no notes-ref target binding".to_owned()));
        return Ok(report);
    };
    let Some(expected_digest) = authorship.note_content_sha256.clone() else {
        report.status = AuthorshipVerificationStatus::NotBound;
        report.reason = authorship
            .reason
            .clone()
            .or_else(|| Some("the selected witness has no note-content binding".to_owned()));
        return Ok(report);
    };
    let note_ref = authorship.note_ref.clone();
    compare_repository_note(
        &mut report,
        repository,
        &note_ref,
        &revision,
        Some(&expected_target),
        &expected_digest,
    );
    Ok(report)
}

/// Verify one repository-native authorship note against a recorded claim.
///
/// The ledger mode above verifies the daemon's settlement-barrier binding on a
/// coder's own result revision. A campaign's merge node binds a *different*
/// object -- the commit the forge minted when it squashed -- and records its
/// claim as a merge receipt in the flow journal, not in the witness ledger.
/// The two mechanisms attest different commits and never meet, so a verifier
/// that can only be pointed at a task lane cannot reach the squash-oid
/// binding at all. This is that path: hand it the receipt's `revision` and
/// `noteSha256` and it re-derives the digest from the repository. Nothing here
/// trusts the claim it is given; the claim is what is being checked.
pub fn verify_revision_authorship(
    repository: &Path,
    note_ref: &str,
    revision: &str,
    expected_digest: &str,
) -> AuthorshipVerificationReport {
    let mut report = AuthorshipVerificationReport {
        note_ref: Some(note_ref.to_owned()),
        result_revision: Some(revision.to_owned()),
        expected_note_content_sha256: Some(expected_digest.to_owned()),
        ..blank_report(repository)
    };
    if !git_oid_shape(revision) {
        report.reason = Some(format!(
            "revision {revision:?} is not a full lowercase Git object ID"
        ));
        return report;
    }
    if !expected_digest
        .strip_prefix("sha256:")
        .is_some_and(|digest| {
            digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    {
        report.reason = Some(format!(
            "expected note digest {expected_digest:?} is not a sha256:<64 hex> value"
        ));
        return report;
    }
    compare_repository_note(
        &mut report,
        repository,
        note_ref,
        revision,
        None,
        expected_digest,
    );
    report
}

/// The shared repository check: the revision resolves, the notes ref is
/// stable across the read, and the note blob for that revision hashes to the
/// digest the caller recorded. `expected_target` is the witnessed notes-ref
/// target when there is one; the revision mode has none to assert.
fn compare_repository_note(
    report: &mut AuthorshipVerificationReport,
    repository: &Path,
    note_ref: &str,
    revision: &str,
    expected_target: Option<&str>,
    expected_digest: &str,
) {
    let revision_probe = match git(repository, ["cat-file", "-e", revision]) {
        Ok(output) => output,
        Err(reason) => return set_error(report, reason),
    };
    if !revision_probe.status.success() {
        report.status = AuthorshipVerificationStatus::RevisionMissing;
        report.reason = Some(format!(
            "result revision {revision} is absent from the supplied repository: {}",
            command_detail(&revision_probe)
        ));
        return;
    }

    let target_before = match resolve_ref(repository, note_ref) {
        Ok(Some(target)) => target,
        Ok(None) => {
            report.status = AuthorshipVerificationStatus::MissingNote;
            report.reason = Some(format!("{note_ref} is absent from the supplied repository"));
            return;
        }
        Err(reason) => return set_error(report, reason),
    };
    report.observed_notes_ref_target = Some(target_before.clone());

    let note_listing = match git(repository, ["notes", "--ref", note_ref, "list", revision]) {
        Ok(output) => output,
        Err(reason) => return set_error(report, reason),
    };
    if !note_listing.status.success() {
        report.status = AuthorshipVerificationStatus::MissingNote;
        report.reason = Some(format!(
            "{note_ref} has no note for {revision}: {}",
            command_detail(&note_listing)
        ));
        return;
    }
    let note_oid = String::from_utf8_lossy(&note_listing.stdout)
        .trim()
        .to_ascii_lowercase();
    if !git_oid_shape(&note_oid) {
        return set_error(
            report,
            format!("git notes returned an invalid note object ID for {revision}: {note_oid:?}"),
        );
    }
    let note = match git(repository, ["cat-file", "blob", note_oid.as_str()]) {
        Ok(output) if output.status.success() => output.stdout,
        Ok(output) => {
            return set_error(
                report,
                format!(
                    "cannot read Git note object {note_oid}: {}",
                    command_detail(&output)
                ),
            )
        }
        Err(reason) => return set_error(report, reason),
    };
    let observed_digest = format!("sha256:{:x}", Sha256::digest(&note));
    report.observed_note_content_sha256 = Some(observed_digest.clone());

    let target_after = match resolve_ref(repository, note_ref) {
        Ok(Some(target)) => target,
        Ok(None) => {
            return set_error(
                report,
                format!("{note_ref} disappeared during verification"),
            )
        }
        Err(reason) => return set_error(report, reason),
    };
    if target_before != target_after {
        report.observed_notes_ref_target = Some(target_after);
        return set_error(
            report,
            format!("{note_ref} changed during verification; retry against a stable repository"),
        );
    }
    if observed_digest != expected_digest {
        report.status = AuthorshipVerificationStatus::NoteContentMismatch;
        report.reason = Some(format!(
            "note content digest is {observed_digest}, expected {expected_digest}"
        ));
        return;
    }
    // A notes ref is an ordinary commit history that grows every time any
    // commit in the repository is annotated -- including the campaign merge
    // node's own post-merge binding on the squash commit. Requiring byte
    // equality with the witnessed target therefore reported a mismatch for
    // every repository that stayed in use after the binding, which is every
    // live repository. What the witness can honestly assert is that the ref
    // only ever moved forward: the witnessed target must still be an ancestor
    // of the observed one. A ref that was rewritten, rolled back, or replaced
    // is not an ancestor and still reports the typed mismatch. The proof
    // itself is unchanged and remains exact -- the note blob for the witnessed
    // revision must hash to the witnessed digest, which is checked above.
    if let Some(expected_target) = expected_target.filter(|target| *target != target_after) {
        let appended = match git(
            repository,
            [
                "merge-base",
                "--is-ancestor",
                expected_target,
                target_after.as_str(),
            ],
        ) {
            Ok(output) => output.status.success(),
            Err(reason) => return set_error(report, reason),
        };
        if !appended {
            report.status = AuthorshipVerificationStatus::NotesRefTargetMismatch;
            report.reason = Some(format!(
                "{note_ref} resolves to {target_after}, which does not descend from the witnessed target {expected_target}"
            ));
            return;
        }
        report.ok = true;
        report.status = AuthorshipVerificationStatus::Match;
        report.reason = Some(format!(
            "the result revision and exact Git AI note binding match; {note_ref} has advanced to {target_after} since the witnessed target {expected_target}"
        ));
        return;
    }

    report.ok = true;
    report.status = AuthorshipVerificationStatus::Match;
    report.reason = Some("the result revision and exact Git AI note binding match".to_owned());
}

fn set_error(report: &mut AuthorshipVerificationReport, reason: String) {
    report.status = AuthorshipVerificationStatus::Error;
    report.reason = Some(reason);
}

fn select_witness<'a>(
    records: &'a [WitnessRecord],
    task_uuid: &str,
    attempt: Option<u32>,
    lease_epoch: Option<u64>,
) -> Option<&'a WitnessRecord> {
    records
        .iter()
        .filter(|record| record.task_uuid.as_deref() == Some(task_uuid))
        .filter(|record| attempt.is_none_or(|value| record.attempt == value))
        .filter(|record| lease_epoch.is_none_or(|value| record.lease_epoch == value))
        .max_by_key(|record| (record.attempt, record.lease_epoch, record.seq))
}

fn resolve_ref(repository: &Path, note_ref: &str) -> Result<Option<String>, String> {
    let output = git(repository, ["rev-parse", "--verify", note_ref])?;
    if !output.status.success() {
        return Ok(None);
    }
    let target = String::from_utf8_lossy(&output.stdout)
        .trim()
        .to_ascii_lowercase();
    if !git_oid_shape(&target) {
        return Err(format!(
            "{note_ref} resolved to an invalid Git object ID: {target:?}"
        ));
    }
    Ok(Some(target))
}

fn git<I, S>(repository: &Path, args: I) -> Result<Output, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .output()
        .map_err(|error| {
            format!(
                "cannot execute git for repository {}: {error}",
                repository.display()
            )
        })
}

fn git_oid_shape(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn command_detail(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.trim();
    if detail.is_empty() {
        format!("exit status {}", output.status)
    } else {
        format!("exit status {}: {detail}", output.status)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use tempfile::tempdir;

    use super::*;
    use crate::taskdb::{AdmissionOrigin, EnqueueSource};
    use crate::witness::{
        Authorship, AuthorshipSession, ChainHead, LaborClass, Verdict, WitnessBody, WitnessLedger,
    };

    fn run_git(repository: &Path, args: &[&str]) -> Output {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn initialize(repository: &Path) -> String {
        fs::create_dir_all(repository).unwrap();
        run_git(repository, &["init", "-q"]);
        run_git(repository, &["config", "user.name", "Tally Test"]);
        run_git(
            repository,
            &["config", "user.email", "tally@example.invalid"],
        );
        fs::write(repository.join("tracked.txt"), "one\n").unwrap();
        run_git(repository, &["add", "tracked.txt"]);
        run_git(repository, &["commit", "-qm", "initial"]);
        String::from_utf8(run_git(repository, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_owned()
    }

    fn install_note(repository: &Path, revision: &str, bytes: &[u8]) {
        let note = repository.join("note");
        fs::write(&note, bytes).unwrap();
        run_git(
            repository,
            &[
                "notes",
                "--ref",
                "refs/notes/ai",
                "add",
                "-f",
                "-F",
                note.to_str().unwrap(),
                revision,
            ],
        );
    }

    fn ref_target(repository: &Path) -> String {
        String::from_utf8(run_git(repository, &["rev-parse", "--verify", "refs/notes/ai"]).stdout)
            .unwrap()
            .trim()
            .to_owned()
    }

    fn append_witness(ledger: &Path, task_uuid: &str, revision: &str, target: &str, note: &[u8]) {
        let mut ledger = WitnessLedger::open(ledger).unwrap();
        ledger
            .append(WitnessBody {
                task_uuid: Some(task_uuid.to_owned()),
                transition_timestamp: "2026-07-26T20:00:00.000Z".to_owned(),
                verdict: Verdict::Pass,
                exit_code: 0,
                artifact_content_hash: None,
                store_paths: None,
                drv: None,
                gpu_seconds: None,
                wall_clock: 1.0,
                attempt: 1,
                lease_epoch: 1,
                dedup_key: None,
                payload_hash: None,
                brief_hash: None,
                origin: AdmissionOrigin::direct(EnqueueSource::Manual),
                orchestration: None,
                labor_class: LaborClass::Fresh,
                trace_ref: None,
                pools: vec!["slot".to_owned()],
                executor: None,
                host_id: None,
                charge: None,
                model: Some("gpt-5".to_owned()),
                evidence_class: None,
                manifest_hash: None,
                completion: None,
                result_revision: Some(revision.to_owned()),
                authorship: Some(Authorship {
                    provider: "git-ai".to_owned(),
                    provider_version: "1.6.17".to_owned(),
                    note_ref: "refs/notes/ai".to_owned(),
                    status: AuthorshipStatus::Bound,
                    notes_ref_target: Some(target.to_owned()),
                    note_content_sha256: Some(format!("sha256:{:x}", Sha256::digest(note))),
                    reason: None,
                }),
                authorship_sessions: Some(vec![AuthorshipSession {
                    tool: "codex".to_owned(),
                    id: "session-53".to_owned(),
                    model: "gpt-5".to_owned(),
                }]),
            })
            .unwrap();
    }

    #[test]
    fn exact_note_binding_matches_without_git_ai_and_mutations_are_precise() {
        let temp = tempdir().unwrap();
        let repository = temp.path().join("repo");
        let ledger = temp.path().join("witness.jsonl");
        let revision = initialize(&repository);
        let note = b"authorship body\n--- git-ai-metadata ---\n{}\n";
        install_note(&repository, &revision, note);
        let target = ref_target(&repository);
        let task_uuid = "00000000-0000-4000-8000-000000000053";
        append_witness(&ledger, task_uuid, &revision, &target, note);

        let matched = verify_authorship(&ledger, &repository, task_uuid, Some(1), Some(1)).unwrap();
        assert!(matched.ok);
        assert_eq!(matched.status, AuthorshipVerificationStatus::Match);
        assert!(matched.ledger.unwrap().ok);

        run_git(
            &repository,
            &[
                "notes",
                "--ref",
                "refs/notes/ai",
                "remove",
                revision.as_str(),
            ],
        );
        let missing = verify_authorship(&ledger, &repository, task_uuid, Some(1), Some(1)).unwrap();
        assert!(!missing.ok);
        assert_eq!(missing.status, AuthorshipVerificationStatus::MissingNote);
        assert!(missing.ledger.unwrap().ok);

        install_note(&repository, &revision, b"replacement\n");
        let replaced =
            verify_authorship(&ledger, &repository, task_uuid, Some(1), Some(1)).unwrap();
        assert_eq!(
            replaced.status,
            AuthorshipVerificationStatus::NoteContentMismatch
        );
        assert_ne!(
            replaced.observed_note_content_sha256,
            replaced.expected_note_content_sha256
        );
        assert!(replaced.ledger.unwrap().ok);
    }

    #[test]
    fn an_appended_note_keeps_the_binding_green_and_a_rewritten_ref_does_not() {
        let temp = tempdir().unwrap();
        let repository = temp.path().join("repo");
        let ledger = temp.path().join("witness.jsonl");
        let revision = initialize(&repository);
        let note = b"bound note\n";
        install_note(&repository, &revision, note);
        let target = ref_target(&repository);
        let task_uuid = "00000000-0000-4000-8000-000000000053";
        append_witness(&ledger, task_uuid, &revision, &target, note);

        // The campaign merge node binds the squash commit on the same notes
        // ref after this result was witnessed. That is an append, so the
        // witnessed proof -- the exact note bytes for the witnessed revision
        // -- is untouched and the binding stays green.
        fs::write(repository.join("other.txt"), "other\n").unwrap();
        run_git(&repository, &["add", "other.txt"]);
        run_git(&repository, &["commit", "-qm", "other"]);
        let other = String::from_utf8(run_git(&repository, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_owned();
        install_note(&repository, &other, b"unrelated note\n");
        let advanced = ref_target(&repository);

        let report = verify_authorship(&ledger, &repository, task_uuid, Some(1), Some(1)).unwrap();
        assert!(report.ok, "{:?}", report.reason);
        assert_eq!(report.status, AuthorshipVerificationStatus::Match);
        assert_eq!(
            report.observed_note_content_sha256,
            report.expected_note_content_sha256
        );
        assert_eq!(
            report.observed_notes_ref_target.as_deref(),
            Some(advanced.as_str())
        );
        assert_ne!(
            report.observed_notes_ref_target,
            report.expected_notes_ref_target
        );
        assert!(report.reason.unwrap().contains("has advanced to"));
        assert!(report.ledger.unwrap().ok);

        // A ref that was rebuilt rather than appended to does not descend from
        // the witnessed target, and that is still the typed mismatch even when
        // the note bytes happen to hash the same.
        run_git(&repository, &["update-ref", "-d", "refs/notes/ai"]);
        install_note(&repository, &other, b"unrelated note\n");
        install_note(&repository, &revision, note);
        assert_ne!(ref_target(&repository), advanced);
        let rebuilt = verify_authorship(&ledger, &repository, task_uuid, Some(1), Some(1)).unwrap();
        assert!(!rebuilt.ok);
        assert_eq!(
            rebuilt.status,
            AuthorshipVerificationStatus::NotesRefTargetMismatch
        );
        assert_eq!(
            rebuilt.observed_note_content_sha256,
            rebuilt.expected_note_content_sha256
        );
        assert!(rebuilt
            .reason
            .unwrap()
            .contains("does not descend from the witnessed target"));
        assert!(rebuilt.ledger.unwrap().ok);
    }

    #[test]
    fn the_revision_mode_reaches_a_binding_the_witness_ledger_never_names() {
        // A campaign's merge node binds the commit the forge minted when it
        // squashed. Nothing about that commit is in the witness ledger, so the
        // only way to check its note is to be pointed at the receipt's own
        // revision and digest.
        let temp = tempdir().unwrap();
        let repository = temp.path().join("repo");
        let revision = initialize(&repository);
        let note = b"squash.txt\n  s_campaign::t_merge 1\n---\n{}\n";
        install_note(&repository, &revision, note);
        let digest = format!("sha256:{:x}", Sha256::digest(note));

        let matched = verify_revision_authorship(&repository, "refs/notes/ai", &revision, &digest);
        assert!(matched.ok, "{:?}", matched.reason);
        assert_eq!(matched.status, AuthorshipVerificationStatus::Match);
        // No ledger was consulted, and none is claimed.
        assert!(matched.ledger.is_none());
        assert!(matched.ledger_path.is_none());
        assert!(matched.task_uuid.is_none());
        assert_eq!(
            matched.observed_note_content_sha256.as_deref(),
            Some(digest.as_str())
        );
        assert_eq!(
            matched.observed_notes_ref_target,
            Some(ref_target(&repository))
        );

        // The digest is the claim being checked, not a decoration: a note the
        // campaign did not publish does not pass under the campaign's digest.
        install_note(&repository, &revision, b"someone else's note\n");
        let replaced = verify_revision_authorship(&repository, "refs/notes/ai", &revision, &digest);
        assert!(!replaced.ok);
        assert_eq!(
            replaced.status,
            AuthorshipVerificationStatus::NoteContentMismatch
        );

        // A ref that grew past the binding is not a mismatch here: the
        // revision mode asserts no notes-ref target of its own.
        install_note(&repository, &revision, note);
        fs::write(repository.join("other.txt"), "other\n").unwrap();
        run_git(&repository, &["add", "other.txt"]);
        run_git(&repository, &["commit", "-qm", "other"]);
        let other = String::from_utf8(run_git(&repository, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_owned();
        install_note(&repository, &other, b"unrelated\n");
        let advanced = verify_revision_authorship(&repository, "refs/notes/ai", &revision, &digest);
        assert!(advanced.ok, "{:?}", advanced.reason);

        // Absent note, absent revision, and a malformed claim are all typed.
        run_git(
            &repository,
            &[
                "notes",
                "--ref",
                "refs/notes/ai",
                "remove",
                revision.as_str(),
            ],
        );
        let missing = verify_revision_authorship(&repository, "refs/notes/ai", &revision, &digest);
        assert_eq!(missing.status, AuthorshipVerificationStatus::MissingNote);
        let absent =
            verify_revision_authorship(&repository, "refs/notes/ai", &"f".repeat(40), &digest);
        assert_eq!(absent.status, AuthorshipVerificationStatus::RevisionMissing);
        for (revision, digest) in [
            ("not-an-oid", digest.as_str()),
            (revision.as_str(), "deadbeef"),
        ] {
            let refused =
                verify_revision_authorship(&repository, "refs/notes/ai", revision, digest);
            assert!(!refused.ok);
            assert_eq!(refused.status, AuthorshipVerificationStatus::Error);
        }
    }

    #[test]
    fn a_missing_result_revision_is_reported_before_note_checks() {
        let temp = tempdir().unwrap();
        let repository = temp.path().join("repo");
        let ledger = temp.path().join("witness.jsonl");
        initialize(&repository);
        let missing_revision = "f".repeat(40);
        let note = b"never reachable\n";
        append_witness(
            &ledger,
            "00000000-0000-4000-8000-000000000053",
            &missing_revision,
            &"e".repeat(40),
            note,
        );

        let report = verify_authorship(
            &ledger,
            &repository,
            "00000000-0000-4000-8000-000000000053",
            None,
            None,
        )
        .unwrap();
        assert_eq!(report.status, AuthorshipVerificationStatus::RevisionMissing);
        assert!(report.ledger.unwrap().ok);
    }

    #[test]
    fn witness_selection_uses_the_latest_matching_lane() {
        let records = vec![
            WitnessRecord {
                attempt: 1,
                lease_epoch: 1,
                seq: 2,
                task_uuid: Some("task".to_owned()),
                ..serde_json::from_value(serde_json::json!({
                    "schemaVersion": 2,
                    "recordType": "verdict",
                    "transitionTimestamp": "2026-07-26T20:00:00.000Z",
                    "verdict": "pass",
                    "exitCode": 0,
                    "gpuSeconds": 0.0,
                    "wallClock": 1.0,
                    "attempt": 1,
                    "leaseEpoch": 1,
                    "origin": {
                        "schemaVersion": 1,
                        "source": "manual",
                        "producer": {"name": "manual", "kind": "manual"}
                    },
                    "laborClass": "fresh",
                    "pools": ["slot"],
                    "seq": 1,
                    "prevHash": format!("sha256:{}", "0".repeat(64)),
                    "hash": format!("sha256:{}", "0".repeat(64))
                }))
                .unwrap()
            },
            WitnessRecord {
                attempt: 2,
                lease_epoch: 3,
                seq: 4,
                task_uuid: Some("task".to_owned()),
                ..serde_json::from_value(serde_json::json!({
                    "schemaVersion": 2,
                    "recordType": "verdict",
                    "transitionTimestamp": "2026-07-26T20:00:00.000Z",
                    "verdict": "pass",
                    "exitCode": 0,
                    "gpuSeconds": 0.0,
                    "wallClock": 1.0,
                    "attempt": 1,
                    "leaseEpoch": 1,
                    "origin": {
                        "schemaVersion": 1,
                        "source": "manual",
                        "producer": {"name": "manual", "kind": "manual"}
                    },
                    "laborClass": "fresh",
                    "pools": ["slot"],
                    "seq": 1,
                    "prevHash": format!("sha256:{}", "0".repeat(64)),
                    "hash": format!("sha256:{}", "0".repeat(64))
                }))
                .unwrap()
            },
        ];
        assert_eq!(
            select_witness(&records, "task", None, None).map(|record| record.seq),
            Some(4)
        );
        assert_eq!(
            select_witness(&records, "task", Some(1), None).map(|record| record.seq),
            Some(2)
        );
    }

    #[test]
    fn chain_head_default_is_unchanged() {
        assert_eq!(ChainHead::default().seq, 0);
    }
}
