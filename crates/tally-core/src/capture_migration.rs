//! The one-shot forward migration for pre-label capture files.
//!
//! Campaign task labels entered `ExecutionIdentity::capture_stem` in #265, in
//! the same edit that changed `unit_stem`. #371 migrated the `unit_stem` half.
//! This is the other half: a row whose orchestration carries a `taskRef` had
//! its captures written by the old binary at `capture/<uuid>.*` and archived at
//! `capture/archive/<uuid>/`, while the current binary derives
//! `capture/<uuid>.<task_id>.*` and `capture/archive/<uuid>.<task_id>/`.
//!
//! # Why this is a repair and not a curiosity
//!
//! `retained_capture_paths` is what `query.run` calls to attach `capturePath`
//! and `stderrTail` to a failure, and it resolves every stream through
//! `capture_stem` with no fallback to the bare-uuid name — the only fallbacks
//! it carries are `.err`-versus-`.adapter.err` suffix ones. The capture
//! *generation* marker is keyed on the bare uuid in both binaries, so it still
//! matches, and that is what makes the loss quiet: the lookup succeeds and
//! reports that the failure has no capture, rather than reporting that it could
//! not find one. An operator reading `tally query run` for a pre-label campaign
//! failure sees no stderr tail and no capture path, and nothing anywhere says
//! the bytes are still on disk.
//!
//! # Why a migration and not a read-path fallback
//!
//! Same policy #371 settled on, for the same reason: strict derivation stays.
//! A permanent bare-uuid fallback in the read path would make every future
//! reader carry a historical naming scheme forever, and would silently resolve
//! a *different* row's capture if a stem ever collided. This is a separate,
//! explicit, idempotent pass an operator runs once.
//!
//! The rename is a pure prefix substitution — `<uuid>.` becomes
//! `<uuid>.<task_id>.` — applied to whatever the old binary left under that
//! stem, so the suffixes are not enumerated here and a stream this module has
//! never heard of moves with the rest. Nothing is rewritten: file contents,
//! modes and mtimes are untouched, `unit-exit/<uuid>.json` and
//! `unit-exit/<uuid>.capture.json` are keyed on the bare uuid in both binaries
//! and are not this migration's business, and the witness ledger is neither
//! read nor written.
//!
//! # What this cannot do
//!
//! It repairs captures on the coordinator only. The labeled stem is derived
//! from the durable rows, and those live exclusively here, so a row dispatched
//! to a remote executor is reported with everything a hand repair on the owning
//! host needs and never claimed as repaired.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

use crate::config::ExecutionTargetConfig;
use crate::executor::{Uuid, CAPTURE_ARCHIVE_DIRECTORY, CAPTURE_DIRECTORY};
use crate::recovery::row_execution_identity;
use crate::taskdb::{read_acknowledged_events, TaskDbError};

pub const CAPTURE_MIGRATION_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum CaptureMigrationError {
    /// A directory that does not exist reads as zero rows, which is
    /// indistinguishable from "nothing to migrate". An operator who mistypes
    /// the path would otherwise get a clean report and conclude their captures
    /// were never stranded.
    #[error("{label} {path} is not a directory; nothing here can be migrated")]
    MissingDirectory { label: &'static str, path: PathBuf },
    #[error("cannot read durable rows from {events_dir}: {source}")]
    TaskDb {
        events_dir: PathBuf,
        source: TaskDbError,
    },
    #[error("cannot read capture directory {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot rename {from} to {to}: {source}")]
    Rename {
        from: PathBuf,
        to: PathBuf,
        source: std::io::Error,
    },
}

/// One capture entry this migration moved, or would move.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenamedCapture {
    pub uuid: Uuid,
    pub from: PathBuf,
    pub to: PathBuf,
}

/// A row or entry this migration did not touch, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedCapture {
    pub uuid: Uuid,
    pub reason: String,
    /// The remote execution target that owns the captures, when they are not
    /// on this host.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executor: Option<String>,
    /// The capture directory on whichever host owns it. `null` when that
    /// host's `stateDir` is not resolvable from this invocation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_dir: Option<PathBuf>,
    /// The stem the old binary used and the one this binary derives, so a hand
    /// repair does not have to rediscover either.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_label_stem: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_stem: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureMigrationReport {
    pub schema_version: u32,
    /// True when the plan was applied; false for a plan-only run.
    pub applied: bool,
    pub state_dir: PathBuf,
    /// Acknowledged rows whose orchestration carries a `taskRef`, which are the
    /// only rows whose capture stem moved.
    pub labeled_rows: usize,
    /// Entries this run renamed, or would rename when `applied` is false.
    pub renamed: Vec<RenamedCapture>,
    /// Labeled rows with no bare-uuid capture entry left. Re-running is a no-op.
    pub already_labeled: usize,
    pub skipped: Vec<SkippedCapture>,
}

impl CaptureMigrationReport {
    /// Whether anything at all still needs moving.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.renamed.is_empty() && self.skipped.is_empty()
    }
}

/// Classify every acknowledged row's capture entries, then — when `apply` —
/// rename the ones still carrying the pre-label stem.
///
/// Classification completes before the first rename, and each rename is a
/// single `rename(2)` within one directory, so an interrupted run leaves every
/// remaining entry exactly as the next run expects to find it. Running it twice
/// is a no-op: the second pass finds no bare-uuid entries and reports
/// `alreadyLabeled`.
pub fn migrate_capture_labels(
    state_dir: &Path,
    executors: &BTreeMap<String, ExecutionTargetConfig>,
    apply: bool,
) -> Result<CaptureMigrationReport, CaptureMigrationError> {
    let events_dir = state_dir.join("events");
    let capture_dir = state_dir.join(CAPTURE_DIRECTORY);
    for (label, path) in [
        ("state directory", state_dir),
        ("durable event directory", events_dir.as_path()),
        ("capture directory", capture_dir.as_path()),
    ] {
        if !path.is_dir() {
            return Err(CaptureMigrationError::MissingDirectory {
                label,
                path: path.to_owned(),
            });
        }
    }
    let events =
        read_acknowledged_events(&events_dir).map_err(|source| CaptureMigrationError::TaskDb {
            events_dir: events_dir.clone(),
            source,
        })?;

    let mut report = CaptureMigrationReport {
        schema_version: CAPTURE_MIGRATION_SCHEMA_VERSION,
        applied: apply,
        state_dir: state_dir.to_owned(),
        labeled_rows: 0,
        renamed: Vec::new(),
        already_labeled: 0,
        skipped: Vec::new(),
    };
    let archive_dir = state_dir.join(CAPTURE_ARCHIVE_DIRECTORY);

    for event in &events {
        let row = &event.row;
        let identity = row_execution_identity(row);
        let expected_stem = identity.capture_stem();
        let pre_label_stem = identity.unit_uuid().to_string();
        // A row with no taskRef derives the same stem under both binaries and
        // has nothing to migrate.
        if expected_stem == pre_label_stem {
            continue;
        }
        report.labeled_rows += 1;

        if let Some(executor) = row.executor.as_deref() {
            let capture_dir = remote_state_dir(executors, executor)
                .map(|state_dir| state_dir.join(CAPTURE_DIRECTORY));
            let location = capture_dir.as_ref().map_or_else(
                || {
                    format!(
                        "under {CAPTURE_DIRECTORY}/ in that executor's stateDir, which this \
                         invocation cannot resolve — read it from the coordinator's \
                         executors.{executor}.stateDir"
                    )
                },
                |path| format!("in {}", path.display()),
            );
            report.skipped.push(SkippedCapture {
                uuid: row.uuid,
                reason: format!(
                    "row is owned by remote executor {executor:?}, whose captures this command \
                     cannot reach: a worker runs no tally daemon and holds no durable rows, so \
                     the labeled stem cannot be derived there. Rename the entries {location} \
                     from the {pre_label_stem:?} stem to the {expected_stem:?} stem by hand."
                ),
                executor: Some(executor.to_owned()),
                capture_dir,
                pre_label_stem: Some(pre_label_stem),
                expected_stem: Some(expected_stem),
            });
            continue;
        }

        let mut planned = Vec::new();
        collect_stream_renames(
            &capture_dir,
            row.uuid,
            &pre_label_stem,
            &expected_stem,
            &mut planned,
            &mut report.skipped,
        )?;
        collect_archive_rename(
            &archive_dir,
            row.uuid,
            &pre_label_stem,
            &expected_stem,
            &mut planned,
            &mut report.skipped,
        );
        if planned.is_empty() {
            report.already_labeled += 1;
            continue;
        }
        report.renamed.append(&mut planned);
    }

    if apply {
        for entry in &report.renamed {
            std::fs::rename(&entry.from, &entry.to).map_err(|source| {
                CaptureMigrationError::Rename {
                    from: entry.from.clone(),
                    to: entry.to.clone(),
                    source,
                }
            })?;
        }
        if !report.renamed.is_empty() {
            // Durability of the names themselves: the bytes never moved.
            for directory in [&capture_dir, &archive_dir] {
                if let Ok(handle) = std::fs::File::open(directory) {
                    let _ = handle.sync_all();
                }
            }
        }
    }
    Ok(report)
}

/// Plan the rename of every current capture stream left under the bare stem.
///
/// The match is the stem plus a separating dot, so `<uuid>.out`,
/// `<uuid>.adapter.err`, `<uuid>.err` and `<uuid>.attempt-N.gates.json` all
/// move without being enumerated, and an entry already carrying the labeled
/// stem — which begins with the same prefix — is excluded explicitly.
fn collect_stream_renames(
    capture_dir: &Path,
    uuid: Uuid,
    pre_label_stem: &str,
    expected_stem: &str,
    planned: &mut Vec<RenamedCapture>,
    skipped: &mut Vec<SkippedCapture>,
) -> Result<(), CaptureMigrationError> {
    let bare_prefix = format!("{pre_label_stem}.");
    let labeled_prefix = format!("{expected_stem}.");
    let entries = std::fs::read_dir(capture_dir).map_err(|source| CaptureMigrationError::Read {
        path: capture_dir.to_owned(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| CaptureMigrationError::Read {
            path: capture_dir.to_owned(),
            source,
        })?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with(&labeled_prefix) || !name.starts_with(&bare_prefix) {
            continue;
        }
        let target = format!("{labeled_prefix}{}", &name[bare_prefix.len()..]);
        let from = capture_dir.join(name);
        let to = capture_dir.join(&target);
        if to.exists() {
            // Both stems present for the same stream is a state this command
            // did not create and will not guess its way out of.
            skipped.push(SkippedCapture {
                uuid,
                reason: format!(
                    "both {name:?} and {target:?} exist in {}; this migration does not choose \
                     between them",
                    capture_dir.display()
                ),
                executor: None,
                capture_dir: None,
                pre_label_stem: None,
                expected_stem: None,
            });
            continue;
        }
        planned.push(RenamedCapture { uuid, from, to });
    }
    planned.sort_by(|left, right| left.from.cmp(&right.from));
    Ok(())
}

/// Plan the rename of the archived-capture directory for one row.
///
/// Absent is the normal case: only a row whose captures were rolled over has
/// one at all.
fn collect_archive_rename(
    archive_dir: &Path,
    uuid: Uuid,
    pre_label_stem: &str,
    expected_stem: &str,
    planned: &mut Vec<RenamedCapture>,
    skipped: &mut Vec<SkippedCapture>,
) {
    let from = archive_dir.join(pre_label_stem);
    if !from.is_dir() {
        return;
    }
    let to = archive_dir.join(expected_stem);
    if to.exists() {
        skipped.push(SkippedCapture {
            uuid,
            reason: format!(
                "both {} and {} exist; this migration does not merge capture archives",
                from.display(),
                to.display()
            ),
            executor: None,
            capture_dir: None,
            pre_label_stem: None,
            expected_stem: None,
        });
        return;
    }
    planned.push(RenamedCapture { uuid, from, to });
}

fn remote_state_dir(
    executors: &BTreeMap<String, ExecutionTargetConfig>,
    executor: &str,
) -> Option<PathBuf> {
    executors
        .get(executor)
        .map(|ExecutionTargetConfig::Ssh(config)| config.state_dir.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{ExecutionIdentity, Executor};
    use crate::taskdb::{write_enqueue_event_atomic, DurableEnqueueEvent};
    use crate::unit_exit_migration::fixtures::row;

    /// The end-to-end repair: an operator-visible capture the current
    /// derivation cannot see becomes one it can, and running the command again
    /// changes nothing.
    #[test]
    fn a_pre_label_campaign_rows_captures_move_to_the_derived_stem_once() {
        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().to_owned();
        let events_dir = state_dir.join("events");
        std::fs::create_dir_all(&events_dir).unwrap();
        let seed = row(Uuid::new_v4(), Some("issue253-live/task-1"));
        let event = DurableEnqueueEvent::new(seed.clone()).unwrap();
        write_enqueue_event_atomic(&events_dir, &event).unwrap();

        let executor = Executor::new(&state_dir, "/nix/store/example/bin/tally");
        let labeled = row_execution_identity(&seed);
        let pre_label = ExecutionIdentity {
            job_id: labeled.job_id,
            task_uuid: labeled.task_uuid,
            task_ref: None,
        };
        assert_ne!(labeled.capture_stem(), pre_label.capture_stem());

        let old = executor.paths(&pre_label);
        std::fs::create_dir_all(old.stdout.parent().unwrap()).unwrap();
        std::fs::write(&old.stdout, b"pre-label stdout\n").unwrap();
        std::fs::write(&old.failure_stderr, b"pre-label failure stderr\n").unwrap();
        std::fs::create_dir_all(old.capture_generation.parent().unwrap()).unwrap();
        std::fs::write(&old.capture_generation, br#"{"attempt":1,"leaseEpoch":7}"#).unwrap();
        let archive = state_dir
            .join(CAPTURE_ARCHIVE_DIRECTORY)
            .join(pre_label.capture_stem());
        std::fs::create_dir_all(&archive).unwrap();
        std::fs::write(
            archive.join("attempt-0000000001-epoch-00000000000000000007.err"),
            b"old\n",
        )
        .unwrap();

        // A plan changes nothing on disk.
        let plan = migrate_capture_labels(&state_dir, &BTreeMap::new(), false).unwrap();
        assert!(!plan.applied);
        assert_eq!(plan.labeled_rows, 1);
        assert_eq!(plan.renamed.len(), 3, "{plan:?}");
        assert!(plan.skipped.is_empty(), "{plan:?}");
        assert!(old.stdout.exists());

        let applied = migrate_capture_labels(&state_dir, &BTreeMap::new(), true).unwrap();
        assert_eq!(applied.renamed.len(), 3);
        let new = executor.paths(&labeled);
        assert_eq!(
            std::fs::read(&new.failure_stderr).unwrap(),
            b"pre-label failure stderr\n"
        );
        assert_eq!(std::fs::read(&new.stdout).unwrap(), b"pre-label stdout\n");
        assert!(!old.stdout.exists());
        assert!(state_dir
            .join(CAPTURE_ARCHIVE_DIRECTORY)
            .join(labeled.capture_stem())
            .join("attempt-0000000001-epoch-00000000000000000007.err")
            .exists());
        // The generation marker is keyed on the bare uuid in both binaries and
        // is deliberately not moved.
        assert!(old.capture_generation.exists());

        // And the capture the operator could not reach is reachable.
        let resolved = executor
            .retained_capture_paths(&labeled, 1, 7)
            .unwrap()
            .expect("the migrated capture resolves under the derived stem");
        assert_eq!(
            std::fs::read(resolved.failure_stderr.unwrap()).unwrap(),
            b"pre-label failure stderr\n"
        );

        // Idempotent.
        let again = migrate_capture_labels(&state_dir, &BTreeMap::new(), true).unwrap();
        assert!(again.is_clean(), "{again:?}");
        assert_eq!(again.already_labeled, 1);
    }

    /// A state directory that is not a coordinator's is refused rather than
    /// reported clean, so a mistyped path cannot masquerade as "nothing to
    /// migrate".
    #[test]
    fn a_directory_without_the_coordinator_trees_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let error = migrate_capture_labels(temp.path(), &BTreeMap::new(), false).unwrap_err();
        assert!(
            matches!(error, CaptureMigrationError::MissingDirectory { .. }),
            "{error:?}"
        );
    }
}
