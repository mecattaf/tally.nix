//! The one-shot forward migration for pre-label `unit-exit/` records.
//!
//! Campaign task labels entered the execution unit name in #265. A row whose
//! orchestration carries a `taskRef` is now `tally-job-{campaign}-{task}-{uuid}`
//! where it used to be `tally-job-{uuid}`, and `UnitExitRecord::validate`
//! compares that name byte for byte. Records written by a binary from before
//! that change therefore name a unit the current binary never derives, and
//! recovery — correctly — refuses them.
//!
//! Strict validation stays. Nothing here makes the old name valid at read time;
//! this is a separate, explicit, idempotent pass that rewrites the durable
//! record to the name the current derivation produces, so the strict read path
//! accepts it forever after. An operator runs it once, named by the startup
//! error, and never thinks about it again.
//!
//! The old value is not preserved anywhere, because it does not need to be: the
//! pre-label name is a pure function of the record's own file name
//! (`unit-exit/<uuid>.json` → `tally-job-<uuid>.service`), so a backup copy
//! would carry no information the surviving file does not. Every other field —
//! `invocationId`, `attempt`, `leaseEpoch`, `serviceResult`, and the exit
//! metadata — round-trips through a `deny_unknown_fields` struct untouched, and
//! the witness ledger is not read or written at all.
//!
//! # What this cannot do
//!
//! It repairs records on the coordinator only. The labeled name is derived from
//! the durable rows, and those live exclusively here: a worker runs no tally
//! daemon and holds no events directory, so running this command on a worker
//! reads zero rows and rewrites nothing. A remote-owned row is therefore
//! reported, never claimed as repaired, and the report carries the executor,
//! the record's absolute path on that host, and both names — everything the
//! hand repair needs and nothing it has to rediscover.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

use crate::config::ExecutionTargetConfig;
use crate::executor::{
    write_exit_record, ExecutorError, UnitExitRecord, Uuid, UNIT_EXIT_DIRECTORY,
};
use crate::recovery::row_execution_identity;
use crate::taskdb::{read_acknowledged_events, TaskDbError};

pub const UNIT_EXIT_MIGRATION_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum UnitExitMigrationError {
    /// A directory that does not exist reads as zero rows, which is
    /// indistinguishable from "nothing to migrate". An operator who mistypes
    /// the path would otherwise get a clean report, restart, and crash-loop
    /// again with no signal that they pointed at the wrong tree.
    #[error("{label} {path} is not a directory; nothing here can be migrated")]
    MissingDirectory { label: &'static str, path: PathBuf },
    #[error(
        "cannot read durable rows from {events_dir}: {source}\n\
         If that refusal names the ordered rowVersion migration, start the daemon once first. \
         Startup runs that migration before recovery reads any exit record, so a daemon that \
         crash-loops on a pre-label record has already brought its rows to the current \
         rowVersion by the time it refuses."
    )]
    TaskDb {
        events_dir: PathBuf,
        source: TaskDbError,
    },
    #[error("cannot read unit exit record {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot rewrite unit exit record {path}: {source}")]
    Write {
        path: PathBuf,
        source: ExecutorError,
    },
}

/// What the migration decided about one acknowledged row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordOutcome {
    pub uuid: Uuid,
    pub path: PathBuf,
    pub recorded_unit: String,
    pub expected_unit: String,
}

/// A row this migration did not touch, and why.
///
/// The structured fields exist because the commonest skip — a row owned by a
/// remote executor — is one this command can never repair, so the report has to
/// carry everything a hand repair on the owning host needs: which host, which
/// file, and both names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedRecord {
    pub uuid: Uuid,
    pub expected_unit: String,
    pub reason: String,
    /// The remote execution target that owns the record, when it is not this
    /// host's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executor: Option<String>,
    /// Absolute path of the record on whichever host owns it. `null` when the
    /// owning host's `stateDir` is not resolvable from this invocation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record_path: Option<PathBuf>,
    /// The name a pre-label record at that path carries. Emitted for remote
    /// rows, whose record this command cannot open to confirm.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_label_unit: Option<String>,
}

impl SkippedRecord {
    fn local(uuid: Uuid, expected_unit: String, reason: String) -> Self {
        Self {
            uuid,
            expected_unit,
            reason,
            executor: None,
            record_path: None,
            pre_label_unit: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnitExitMigrationReport {
    pub schema_version: u32,
    /// True when the plan was written to disk; false for a plan-only run.
    pub applied: bool,
    pub state_dir: PathBuf,
    /// Acknowledged rows whose orchestration carries a `taskRef`, which are the
    /// only rows whose unit name moved.
    pub labeled_rows: usize,
    /// Records this run rewrote, or would rewrite when `applied` is false.
    pub rewritten: Vec<RecordOutcome>,
    /// Records already carrying the labeled name. Re-running is a no-op.
    pub already_labeled: usize,
    pub skipped: Vec<SkippedRecord>,
}

impl UnitExitMigrationReport {
    /// Whether anything at all still needs writing.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.rewritten.is_empty() && self.skipped.is_empty()
    }
}

/// Classify every acknowledged row, then — when `apply` — rewrite the records
/// that carry the pre-label name.
///
/// Classification completes before the first mutation, and each replacement is
/// atomic, so an interrupted run leaves every remaining record exactly as the
/// next run expects to find it. Running it twice is a no-op: the second pass
/// sees the labeled name and reports `alreadyLabeled`.
pub fn migrate_unit_exit_labels(
    state_dir: &Path,
    executors: &BTreeMap<String, ExecutionTargetConfig>,
    apply: bool,
) -> Result<UnitExitMigrationReport, UnitExitMigrationError> {
    let events_dir = state_dir.join("events");
    let unit_exit_dir = state_dir.join(UNIT_EXIT_DIRECTORY);
    // Both directories are created by daemon startup, so their absence means
    // this is not a coordinator state directory — not that there is no work.
    for (label, path) in [
        ("state directory", state_dir),
        ("durable event directory", events_dir.as_path()),
    ] {
        if !path.is_dir() {
            return Err(UnitExitMigrationError::MissingDirectory {
                label,
                path: path.to_owned(),
            });
        }
    }
    let events =
        read_acknowledged_events(&events_dir).map_err(|source| UnitExitMigrationError::TaskDb {
            events_dir: events_dir.clone(),
            source,
        })?;

    let mut report = UnitExitMigrationReport {
        schema_version: UNIT_EXIT_MIGRATION_SCHEMA_VERSION,
        applied: apply,
        state_dir: state_dir.to_owned(),
        labeled_rows: 0,
        rewritten: Vec::new(),
        already_labeled: 0,
        skipped: Vec::new(),
    };
    let mut planned = Vec::new();

    for event in &events {
        let row = &event.row;
        let identity = row_execution_identity(row);
        let expected_unit = identity.unit_name();
        let pre_label_unit = identity.pre_label_unit_name();
        // A row with no taskRef derives the same name under both binaries and
        // has nothing to migrate.
        if expected_unit == pre_label_unit {
            continue;
        }
        report.labeled_rows += 1;

        if let Some(executor) = row.executor.as_deref() {
            // This command cannot repair a remote-owned record, and must not
            // pretend otherwise. The labeled name is derived from the durable
            // rows, and the rows live only here: a worker runs no tally daemon
            // and has no events directory, so the same command run there reads
            // zero rows and rewrites nothing. What it can do is hand the
            // operator everything the manual repair needs.
            let record_path = remote_state_dir(executors, executor).map(|state_dir| {
                state_dir
                    .join(UNIT_EXIT_DIRECTORY)
                    .join(format!("{}.json", row.uuid))
            });
            let location = record_path.as_ref().map_or_else(
                || {
                    format!(
                        "under {UNIT_EXIT_DIRECTORY}/{}.json in that executor's stateDir, which \
                         this invocation cannot resolve — read it from the coordinator's \
                         executors.{executor}.stateDir",
                        row.uuid
                    )
                },
                |path| format!("at {}", path.display()),
            );
            report.skipped.push(SkippedRecord {
                uuid: row.uuid,
                reason: format!(
                    "row is owned by remote executor {executor:?}, whose records this command \
                     cannot reach or repair: a worker runs no tally daemon and holds no durable \
                     rows, so the labeled name cannot be derived there. Rewrite the record {location} \
                     on that host by hand, changing its \"unit\" field from {pre_label_unit:?} to \
                     {expected_unit:?} and nothing else."
                ),
                expected_unit,
                executor: Some(executor.to_owned()),
                record_path,
                pre_label_unit: Some(pre_label_unit),
            });
            continue;
        }

        let path = unit_exit_dir.join(format!("{}.json", row.uuid));
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => return Err(UnitExitMigrationError::Read { path, source }),
        };
        let record: UnitExitRecord = match serde_json::from_slice(&bytes) {
            Ok(record) => record,
            Err(error) => {
                // Not this migration's business, and not a reason to abandon
                // the rest of the pass. Recovery still refuses this record and
                // still says why.
                report.skipped.push(SkippedRecord::local(
                    row.uuid,
                    expected_unit,
                    format!(
                        "record at {} is not a unit exit record: {error}",
                        path.display()
                    ),
                ));
                continue;
            }
        };
        if record.unit == expected_unit {
            report.already_labeled += 1;
            continue;
        }
        if record.unit != pre_label_unit {
            report.skipped.push(SkippedRecord::local(
                row.uuid,
                expected_unit,
                format!(
                    "record names unit {:?}, which is neither the pre-label name {pre_label_unit:?} \
                     nor the expected name; this migration does not guess",
                    record.unit
                ),
            ));
            continue;
        }
        report.rewritten.push(RecordOutcome {
            uuid: row.uuid,
            path: path.clone(),
            recorded_unit: record.unit.clone(),
            expected_unit: expected_unit.clone(),
        });
        planned.push((path, record, expected_unit));
    }

    report.rewritten.sort_by_key(|outcome| outcome.uuid);
    report.skipped.sort_by_key(|skipped| skipped.uuid);

    if apply {
        for (path, mut record, expected_unit) in planned {
            record.unit = expected_unit;
            write_exit_record(&path, &record)
                .map_err(|source| UnitExitMigrationError::Write { path, source })?;
        }
    }
    Ok(report)
}

/// The `stateDir` the coordinator dispatches to for one named executor.
///
/// This is the only place the worker's own layout is known, which is why the
/// migration takes the executors map at all: without it the report can name the
/// host but not the file.
fn remote_state_dir(
    executors: &BTreeMap<String, ExecutionTargetConfig>,
    executor: &str,
) -> Option<PathBuf> {
    executors
        .get(executor)
        .map(|ExecutionTargetConfig::Ssh(config)| config.state_dir.clone())
}

/// Row seeds and durable events shaped exactly as the daemon writes them, so
/// the migration and its callers exercise the real read path.
#[cfg(test)]
pub(crate) mod fixtures {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use serde_json::json;

    use crate::config::Priority;
    use crate::executor::{write_exit_record, UnitExitRecord, Uuid, UNIT_EXIT_DIRECTORY};
    use crate::provenance::Orchestration;
    use crate::taskdb::{
        write_enqueue_event_atomic, AdmissionOrigin, DurableEnqueueEvent, EnqueueSource, RowSeed,
    };

    pub(crate) fn row(uuid: Uuid, task_ref: Option<&str>) -> RowSeed {
        RowSeed {
            row_version: crate::taskdb::CURRENT_ROW_VERSION,
            uuid,
            description: "recover this durable leaf".to_owned(),
            priority: Priority::High,
            source: EnqueueSource::EventsDir,
            adapter: "shell".to_owned(),
            pools: vec!["worker".to_owned()],
            executor: None,
            model: None,
            cwd: Some(PathBuf::from("/work")),
            workspace: None,
            adapter_options: Default::default(),
            gate_manifest: None,
            resumed_from: None,
            dedup_key: Some(format!("dedup:{uuid}")),
            payload_hash: None,
            brief_hash: None,
            orchestration: task_ref.map(|task_ref| {
                serde_json::from_value::<Orchestration>(json!({
                    "flowRunId": "018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321",
                    "taskRef": task_ref,
                }))
                .unwrap()
            }),
            session_ref: None,
            session_cwd: None,
            final_message: None,
            job_token_hash: None,
            lease_epoch: 3,
            attempt: 1,
            argv: vec!["worker".to_owned(), "leaf".to_owned()],
            evidence: vec!["exit:0".to_owned()],
            drv: None,
            parent_uuid: None,
            consumption_estimate: Some(3),
            runtime_max_sec: Some(30),
            no_enqueue: false,
            credentials: BTreeMap::new(),
            origin: Some(AdmissionOrigin::direct(EnqueueSource::EventsDir)),
            gh_origin: None,
            related_trigger: None,
            evidence_class: None,
            manifest_hash: None,
            usage: None,
            context_tokens: None,
            context_window: None,
        }
    }

    pub(crate) fn exit_record(unit: &str) -> UnitExitRecord {
        UnitExitRecord {
            accounting: None,
            schema_version: crate::executor::UNIT_EXIT_SCHEMA_VERSION,
            unit: unit.to_owned(),
            invocation_id: "5f3b1a2c4d6e7f8a".to_owned(),
            attempt: 1,
            lease_epoch: 3,
            service_result: "success".to_owned(),
            exit_code: Some("exited".to_owned()),
            exit_status: Some("0".to_owned()),
        }
    }

    /// A dispatch target whose only interesting field here is its `stateDir`.
    pub(crate) fn ssh_executor(state_dir: &str) -> crate::config::SshExecutorConfig {
        crate::config::SshExecutorConfig {
            host: "worker.invalid".to_owned(),
            user: "tally-worker".to_owned(),
            port: 22,
            ssh_program: PathBuf::from("/bin/ssh"),
            identity_file: PathBuf::from("/key"),
            known_hosts_file: PathBuf::from("/known-hosts"),
            program: PathBuf::from("/bin/tally"),
            state_dir: PathBuf::from(state_dir),
            connect_timeout_sec: 1,
            server_alive_interval_sec: 1,
            server_alive_count_max: 1,
            retry_interval_ms: 10,
        }
    }

    /// Write one acknowledged event plus the `unit-exit` record a binary of the
    /// given vintage would have left behind, and return the record's path.
    pub(crate) fn seed(state_dir: &Path, row: &RowSeed, recorded_unit: &str) -> PathBuf {
        let events_dir = state_dir.join("events");
        std::fs::create_dir_all(&events_dir).unwrap();
        let event = DurableEnqueueEvent::new(row.clone()).unwrap();
        assert!(event.acknowledged);
        write_enqueue_event_atomic(&events_dir, &event).unwrap();
        let path = state_dir
            .join(UNIT_EXIT_DIRECTORY)
            .join(format!("{}.json", row.uuid));
        write_exit_record(&path, &exit_record(recorded_unit)).unwrap();
        path
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{row, seed};
    use super::*;
    use crate::executor::Executor;

    fn read(path: &Path) -> UnitExitRecord {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn a_pre_label_record_is_rewritten_once_and_then_left_alone() {
        let temp = tempfile::tempdir().unwrap();
        let row = row(Uuid::new_v4(), Some("issue253-live/task-1"));
        let identity = row_execution_identity(&row);
        let path = seed(temp.path(), &row, &identity.pre_label_unit_name());

        let plan = migrate_unit_exit_labels(temp.path(), &BTreeMap::new(), false).unwrap();
        assert_eq!(plan.labeled_rows, 1);
        assert_eq!(plan.rewritten.len(), 1);
        assert!(!plan.applied);
        let before = read(&path);
        assert_eq!(
            before.unit,
            identity.pre_label_unit_name(),
            "a plan-only run must not write"
        );

        let applied = migrate_unit_exit_labels(temp.path(), &BTreeMap::new(), true).unwrap();
        assert_eq!(applied.rewritten.len(), 1);
        let after = read(&path);
        assert_eq!(after.unit, identity.unit_name());
        // Every semantic fact survives the rename.
        assert_eq!(after.invocation_id, before.invocation_id);
        assert_eq!(after.attempt, before.attempt);
        assert_eq!(after.lease_epoch, before.lease_epoch);
        assert_eq!(after.service_result, before.service_result);
        assert_eq!(after.exit_code, before.exit_code);
        assert_eq!(after.exit_status, before.exit_status);

        let again = migrate_unit_exit_labels(temp.path(), &BTreeMap::new(), true).unwrap();
        assert!(again.rewritten.is_empty());
        assert_eq!(again.already_labeled, 1);
        assert!(again.is_clean());
    }

    #[test]
    fn an_unlabeled_row_and_a_foreign_name_are_never_rewritten() {
        let temp = tempfile::tempdir().unwrap();
        let plain = row(Uuid::new_v4(), None);
        seed(
            temp.path(),
            &plain,
            &row_execution_identity(&plain).unit_name(),
        );

        let foreign = row(Uuid::new_v4(), Some("issue253-live/task-2"));
        let foreign_path = seed(temp.path(), &foreign, "tally-job-something-else.service");

        let report = migrate_unit_exit_labels(temp.path(), &BTreeMap::new(), true).unwrap();
        assert_eq!(report.labeled_rows, 1, "only the taskRef row is in scope");
        assert!(report.rewritten.is_empty());
        assert_eq!(report.skipped.len(), 1);
        assert!(report.skipped[0]
            .reason
            .contains("this migration does not guess"));
        assert_eq!(read(&foreign_path).unit, "tally-job-something-else.service");
    }

    /// A worker holds no durable rows, so neither this command here nor the
    /// same command run there can repair a remote-owned record. What the report
    /// owes the operator instead is every fact the hand repair needs: which
    /// host, which file on it, and both names.
    #[test]
    fn a_remote_row_is_reported_with_the_facts_a_hand_repair_needs() {
        let temp = tempfile::tempdir().unwrap();
        let mut remote = row(Uuid::new_v4(), Some("issue253-live/task-3"));
        remote.executor = Some("worker".to_owned());
        let identity = row_execution_identity(&remote);
        let path = seed(temp.path(), &remote, &identity.pre_label_unit_name());
        let executors = BTreeMap::from([(
            "worker".to_owned(),
            ExecutionTargetConfig::Ssh(fixtures::ssh_executor("/var/lib/tally-worker/state")),
        )]);

        let report = migrate_unit_exit_labels(temp.path(), &executors, true).unwrap();
        assert!(report.rewritten.is_empty());
        assert_eq!(report.skipped.len(), 1);
        let skipped = &report.skipped[0];
        assert_eq!(skipped.executor.as_deref(), Some("worker"));
        assert_eq!(
            skipped.record_path.as_deref(),
            Some(
                Path::new("/var/lib/tally-worker/state")
                    .join(UNIT_EXIT_DIRECTORY)
                    .join(format!("{}.json", remote.uuid))
                    .as_path()
            ),
            "the report must name the record's path on the host that owns it"
        );
        assert_eq!(
            skipped.pre_label_unit.as_deref(),
            Some(identity.pre_label_unit_name().as_str())
        );
        assert_eq!(skipped.expected_unit, identity.unit_name());
        assert!(
            skipped.reason.contains("cannot reach or repair"),
            "the reason must not promise a repair: {}",
            skipped.reason
        );
        assert!(
            !skipped.reason.contains("run this migration against"),
            "the retracted claim must be gone: {}",
            skipped.reason
        );
        assert_eq!(read(&path).unit, identity.pre_label_unit_name());
    }

    /// Without a configuration the executor is still named; only the path is
    /// unknown, and the reason says where to read it from.
    #[test]
    fn an_unresolvable_executor_names_where_its_state_dir_is_configured() {
        let temp = tempfile::tempdir().unwrap();
        let mut remote = row(Uuid::new_v4(), Some("issue253-live/task-5"));
        remote.executor = Some("worker".to_owned());
        seed(
            temp.path(),
            &remote,
            &row_execution_identity(&remote).pre_label_unit_name(),
        );

        let report = migrate_unit_exit_labels(temp.path(), &BTreeMap::new(), true).unwrap();
        let skipped = &report.skipped[0];
        assert_eq!(skipped.executor.as_deref(), Some("worker"));
        assert!(skipped.record_path.is_none());
        assert!(skipped.reason.contains("executors.worker.stateDir"));
    }

    /// A mistyped path used to read as zero rows and answer clean, which is the
    /// failure where an operator runs the documented command, sees `ok`,
    /// restarts, and crash-loops again with no signal.
    #[test]
    fn a_directory_that_is_not_a_coordinator_state_tree_is_refused() {
        let temp = tempfile::tempdir().unwrap();

        let absent = temp.path().join("stat");
        let error = migrate_unit_exit_labels(&absent, &BTreeMap::new(), true)
            .expect_err("a missing state directory must not read as clean");
        assert!(matches!(
            error,
            UnitExitMigrationError::MissingDirectory {
                label: "state directory",
                ..
            }
        ));

        // A worker's state directory: unit-exit records, and no events/.
        let worker = temp.path().join("worker");
        std::fs::create_dir_all(worker.join(UNIT_EXIT_DIRECTORY)).unwrap();
        let error = migrate_unit_exit_labels(&worker, &BTreeMap::new(), true)
            .expect_err("a worker state directory holds no rows and must not read as clean");
        assert!(matches!(
            error,
            UnitExitMigrationError::MissingDirectory {
                label: "durable event directory",
                ..
            }
        ));
    }

    #[test]
    fn the_migration_targets_exactly_the_path_the_executor_reads() {
        let temp = tempfile::tempdir().unwrap();
        let row = row(Uuid::new_v4(), Some("issue253-live/task-4"));
        let path = seed(
            temp.path(),
            &row,
            &row_execution_identity(&row).pre_label_unit_name(),
        );
        let executor = Executor::new(temp.path(), "/bin/tally");
        assert_eq!(
            executor.paths(&row_execution_identity(&row)).exit_record,
            path
        );
    }
}
