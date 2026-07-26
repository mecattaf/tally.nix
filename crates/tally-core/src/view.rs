use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use fs2::FileExt;
use serde::Serialize;
use thiserror::Error;

use crate::taskdb::{RebuildStats, TaskDb, TaskDbError, TASKDATA_DIRECTORY};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewRebuildReport {
    pub rebuilt: bool,
    pub rows: usize,
    pub witness_records: usize,
}

#[derive(Debug, Error)]
pub enum ViewRebuildError {
    #[error("cannot rebuild the TaskChampion view while the daemon lock is held at {path}")]
    DaemonLockHeld { path: PathBuf },
    #[error("view rebuild path must be absolute: {path}")]
    RelativePath { path: PathBuf },
    #[error("view rebuild archive already exists at {path}")]
    ArchiveExists { path: PathBuf },
    #[error("view rebuild I/O error at {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("view rebuild failed: {0}")]
    TaskDb(#[from] TaskDbError),
}

fn io_error(path: &Path, source: io::Error) -> ViewRebuildError {
    ViewRebuildError::Io {
        path: path.to_owned(),
        source,
    }
}

fn acquire_daemon_guard(state_dir: &Path) -> Result<File, ViewRebuildError> {
    std::fs::create_dir_all(state_dir).map_err(|source| io_error(state_dir, source))?;
    let path = state_dir.join("daemon.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&path)
        .map_err(|source| io_error(&path, source))?;
    file.try_lock_exclusive().map_err(|source| {
        if source.kind() == io::ErrorKind::WouldBlock {
            ViewRebuildError::DaemonLockHeld { path: path.clone() }
        } else {
            io_error(&path, source)
        }
    })?;
    Ok(file)
}

fn path_entry_exists(path: &Path) -> Result<bool, ViewRebuildError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io_error(path, source)),
    }
}

fn archive_path(taskdata_dir: &Path, now: DateTime<Utc>) -> PathBuf {
    PathBuf::from(format!(
        "{}.pre-rebuild-{}",
        taskdata_dir.display(),
        now.to_rfc3339_opts(SecondsFormat::Millis, true)
    ))
}

fn archive_existing_view(
    data_dir: &Path,
    now: DateTime<Utc>,
) -> Result<Option<PathBuf>, ViewRebuildError> {
    let taskdata_dir = data_dir.join(TASKDATA_DIRECTORY);
    if !path_entry_exists(&taskdata_dir)? {
        return Ok(None);
    }
    let archive = archive_path(&taskdata_dir, now);
    if path_entry_exists(&archive)? {
        return Err(ViewRebuildError::ArchiveExists { path: archive });
    }
    std::fs::rename(&taskdata_dir, &archive).map_err(|source| io_error(&taskdata_dir, source))?;
    File::open(data_dir)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error(data_dir, source))?;
    Ok(Some(archive))
}

pub async fn rebuild_taskchampion_view(
    state_dir: &Path,
    data_dir: &Path,
    now: DateTime<Utc>,
) -> Result<ViewRebuildReport, ViewRebuildError> {
    for path in [state_dir, data_dir] {
        if !path.is_absolute() {
            return Err(ViewRebuildError::RelativePath {
                path: path.to_owned(),
            });
        }
    }

    let _daemon_guard = acquire_daemon_guard(state_dir)?;
    std::fs::create_dir_all(data_dir).map_err(|source| io_error(data_dir, source))?;
    archive_existing_view(data_dir, now)?;

    let mut db = TaskDb::open(data_dir).await?;
    let RebuildStats {
        rows,
        witness_records,
    } = db
        .rebuild_from_sources_with_stats(&state_dir.join("events"), &data_dir.join("witness.jsonl"))
        .await?;
    Ok(ViewRebuildReport {
        rebuilt: true,
        rows,
        witness_records,
    })
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn archive_name_is_rfc3339_and_never_replaces_an_existing_entry() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let taskdata = data_dir.join(TASKDATA_DIRECTORY);
        std::fs::create_dir(&taskdata).unwrap();
        std::fs::write(taskdata.join("sentinel"), b"original").unwrap();
        let now = Utc
            .with_ymd_and_hms(2026, 7, 26, 18, 30, 0)
            .single()
            .unwrap();

        let archived = archive_existing_view(&data_dir, now).unwrap().unwrap();
        assert_eq!(
            archived.file_name().unwrap(),
            "taskdata.pre-rebuild-2026-07-26T18:30:00.000Z"
        );
        assert_eq!(
            std::fs::read(archived.join("sentinel")).unwrap(),
            b"original"
        );

        std::fs::create_dir(&taskdata).unwrap();
        let error = archive_existing_view(&data_dir, now).unwrap_err();
        assert!(matches!(error, ViewRebuildError::ArchiveExists { .. }));
        assert!(taskdata.is_dir());
    }
}
