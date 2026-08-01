use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::{StorageBudgetConfig, StorageConfig};
use crate::taskdb::TASKDATA_DIRECTORY;

pub const STORAGE_STATE_FILE: &str = "storage-metrics.json";
pub const STORAGE_WARNING_FILE: &str = "storage-warnings.jsonl";
const TASKCHAMPION_DB: &str = "taskchampion.sqlite3";

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("storage monitor I/O error at {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("storage monitor JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

fn io_error(path: &Path, source: io::Error) -> StorageError {
    StorageError::Io {
        path: path.to_owned(),
        source,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BudgetLevel {
    Ok,
    Warning,
    Hard,
}

impl BudgetLevel {
    fn for_size(bytes: u64, budget: &StorageBudgetConfig) -> Self {
        if bytes >= budget.hard_bytes {
            Self::Hard
        } else if bytes >= budget.warning_bytes {
            Self::Warning
        } else {
            Self::Ok
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreMetrics {
    pub size_bytes: u64,
    pub apparent_bytes: u64,
    pub file_count: u64,
    pub warning_bytes: u64,
    pub hard_bytes: u64,
    pub level: BudgetLevel,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskchampionMetrics {
    pub database_bytes: u64,
    pub wal_bytes: u64,
    pub shm_bytes: u64,
    pub total_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_high_water: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntakeStatus {
    pub accepting: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrowthPerCompletion {
    pub completion_delta: u64,
    pub data_dir_bytes: i64,
    pub state_dir_bytes: i64,
    pub taskchampion_bytes: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub taskchampion_operations: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveStorageWarning {
    pub warning_sequence: u64,
    pub store: String,
    pub level: BudgetLevel,
    pub size_bytes: u64,
    pub threshold_bytes: u64,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageMetrics {
    pub schema_version: u32,
    pub sampled_at: String,
    pub completion_count: u64,
    pub intake: IntakeStatus,
    pub data_dir: StoreMetrics,
    pub state_dir: StoreMetrics,
    pub taskchampion: TaskchampionMetrics,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub growth_per_completion: Option<GrowthPerCompletion>,
    pub active_warnings: Vec<ActiveStorageWarning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageWarningRecord {
    pub schema_version: u32,
    pub sequence: u64,
    pub recorded_at: String,
    pub store: String,
    pub previous_level: BudgetLevel,
    pub level: BudgetLevel,
    pub size_bytes: u64,
    pub threshold_bytes: u64,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoragePoint {
    sampled_at: String,
    completion_count: u64,
    data_dir_bytes: u64,
    state_dir_bytes: u64,
    taskchampion_bytes: u64,
    taskchampion_operations: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PersistentState {
    schema_version: u32,
    previous: Option<StoragePoint>,
    current: StoragePoint,
    data_dir_level: BudgetLevel,
    state_dir_level: BudgetLevel,
    next_warning_sequence: u64,
    #[serde(default)]
    data_dir_episode: Option<u64>,
    #[serde(default)]
    state_dir_episode: Option<u64>,
}

pub struct StorageMonitor {
    data_dir: PathBuf,
    state_dir: PathBuf,
    config: StorageConfig,
    state: Option<PersistentState>,
    snapshot: Option<StorageMetrics>,
    pending_warnings: Vec<StorageWarningRecord>,
    last_error: Option<String>,
}

impl StorageMonitor {
    pub fn open(
        data_dir: impl Into<PathBuf>,
        state_dir: impl Into<PathBuf>,
        config: StorageConfig,
        completion_count: u64,
    ) -> Result<Self, StorageError> {
        let data_dir = data_dir.into();
        let state_dir = state_dir.into();
        let state_path = data_dir.join(STORAGE_STATE_FILE);
        let state = match std::fs::read(&state_path) {
            Ok(bytes) => Some(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(source) => return Err(io_error(&state_path, source)),
        };
        let mut monitor = Self {
            data_dir,
            state_dir,
            config,
            state,
            snapshot: None,
            pending_warnings: Vec::new(),
            last_error: None,
        };
        monitor.refresh(completion_count)?;
        Ok(monitor)
    }

    pub fn poll_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.config.poll_interval_sec)
    }

    pub fn refresh(&mut self, completion_count: u64) -> Result<&StorageMetrics, StorageError> {
        match self.sample(completion_count) {
            Ok(snapshot) => {
                self.last_error = None;
                self.snapshot = Some(snapshot);
                Ok(self.snapshot.as_ref().expect("snapshot set above"))
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
                Err(error)
            }
        }
    }

    pub fn snapshot(&self) -> Option<&StorageMetrics> {
        self.snapshot.as_ref()
    }

    pub fn query_snapshot(&self) -> Option<StorageMetrics> {
        let mut snapshot = self.snapshot.clone()?;
        if let Some(error) = &self.last_error {
            snapshot.monitor_error = Some(error.clone());
            snapshot.intake = IntakeStatus {
                accepting: false,
                reason: Some(format!(
                    "storage monitor is unavailable: {error}; new intake is refused while already-admitted work continues"
                )),
            };
        }
        Some(snapshot)
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn take_warnings(&mut self) -> Vec<StorageWarningRecord> {
        std::mem::take(&mut self.pending_warnings)
    }

    fn sample(&mut self, completion_count: u64) -> Result<StorageMetrics, StorageError> {
        let sampled_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let data_usage = directory_usage(&self.data_dir)?;
        let state_usage = directory_usage(&self.state_dir)?;
        let taskchampion = taskchampion_metrics(&self.data_dir);
        let data_level = BudgetLevel::for_size(data_usage.allocated, &self.config.data_dir);
        let state_level = BudgetLevel::for_size(state_usage.allocated, &self.config.state_dir);
        let point = StoragePoint {
            sampled_at: sampled_at.clone(),
            completion_count,
            data_dir_bytes: data_usage.allocated,
            state_dir_bytes: state_usage.allocated,
            taskchampion_bytes: taskchampion.total_bytes,
            taskchampion_operations: taskchampion.operation_high_water,
        };

        let (
            previous,
            old_data_level,
            old_state_level,
            next_warning_sequence,
            mut data_episode,
            mut state_episode,
        ) = self.state.as_ref().map_or(
            (None, BudgetLevel::Ok, BudgetLevel::Ok, 1, None, None),
            |state| {
                let previous = if completion_count > state.current.completion_count {
                    Some(state.current.clone())
                } else if completion_count == state.current.completion_count {
                    state.previous.clone()
                } else {
                    None
                };
                (
                    previous,
                    state.data_dir_level,
                    state.state_dir_level,
                    state.next_warning_sequence,
                    state.data_dir_episode,
                    state.state_dir_episode,
                )
            },
        );
        let mut next_sequence = next_warning_sequence;
        for (store, previous_level, level, usage, budget) in [
            (
                "dataDir",
                old_data_level,
                data_level,
                data_usage.allocated,
                &self.config.data_dir,
            ),
            (
                "stateDir",
                old_state_level,
                state_level,
                state_usage.allocated,
                &self.config.state_dir,
            ),
        ] {
            if previous_level != level {
                let threshold = match level {
                    BudgetLevel::Ok => budget.warning_bytes,
                    BudgetLevel::Warning => budget.warning_bytes,
                    BudgetLevel::Hard => budget.hard_bytes,
                };
                let message = transition_message(store, level, usage, budget);
                let record = StorageWarningRecord {
                    schema_version: 1,
                    sequence: next_sequence,
                    recorded_at: sampled_at.clone(),
                    store: store.to_owned(),
                    previous_level,
                    level,
                    size_bytes: usage,
                    threshold_bytes: threshold,
                    message,
                };
                append_warning(&self.data_dir, &record)?;
                self.pending_warnings.push(record);
                if store == "dataDir" {
                    data_episode = Some(next_sequence);
                } else {
                    state_episode = Some(next_sequence);
                }
                next_sequence += 1;
            }
        }

        let persistent = PersistentState {
            schema_version: 1,
            previous: previous.clone(),
            current: point.clone(),
            data_dir_level: data_level,
            state_dir_level: state_level,
            next_warning_sequence: next_sequence,
            data_dir_episode: data_episode,
            state_dir_episode: state_episode,
        };
        write_json_atomic(&self.data_dir.join(STORAGE_STATE_FILE), &persistent)?;
        self.state = Some(persistent);

        let mut active_warnings = Vec::new();
        push_active_warning(
            &mut active_warnings,
            "dataDir",
            data_level,
            data_usage.allocated,
            &self.config.data_dir,
            data_episode,
        );
        push_active_warning(
            &mut active_warnings,
            "stateDir",
            state_level,
            state_usage.allocated,
            &self.config.state_dir,
            state_episode,
        );
        let hard = active_warnings
            .iter()
            .filter(|warning| warning.level == BudgetLevel::Hard)
            .map(|warning| warning.message.clone())
            .collect::<Vec<_>>();
        let intake = if hard.is_empty() {
            IntakeStatus {
                accepting: true,
                reason: None,
            }
        } else {
            IntakeStatus {
                accepting: false,
                reason: Some(format!(
                    "{}; new intake is refused while already-admitted work continues",
                    hard.join("; ")
                )),
            }
        };

        Ok(StorageMetrics {
            schema_version: 1,
            sampled_at,
            completion_count,
            intake,
            data_dir: StoreMetrics {
                size_bytes: data_usage.allocated,
                apparent_bytes: data_usage.apparent,
                file_count: data_usage.files,
                warning_bytes: self.config.data_dir.warning_bytes,
                hard_bytes: self.config.data_dir.hard_bytes,
                level: data_level,
            },
            state_dir: StoreMetrics {
                size_bytes: state_usage.allocated,
                apparent_bytes: state_usage.apparent,
                file_count: state_usage.files,
                warning_bytes: self.config.state_dir.warning_bytes,
                hard_bytes: self.config.state_dir.hard_bytes,
                level: state_level,
            },
            taskchampion,
            growth_per_completion: previous
                .as_ref()
                .and_then(|previous| growth(previous, &point)),
            active_warnings,
            monitor_error: None,
        })
    }
}

fn transition_message(
    store: &str,
    level: BudgetLevel,
    size: u64,
    budget: &StorageBudgetConfig,
) -> String {
    match level {
        BudgetLevel::Ok => format!(
            "tally {store} recovered below its storage warning budget: {size} < {} bytes",
            budget.warning_bytes
        ),
        BudgetLevel::Warning => format!(
            "tally {store} crossed its storage warning budget: {size} >= {} bytes",
            budget.warning_bytes
        ),
        BudgetLevel::Hard => format!(
            "tally {store} crossed its hard storage budget: {size} >= {} bytes",
            budget.hard_bytes
        ),
    }
}

fn push_active_warning(
    warnings: &mut Vec<ActiveStorageWarning>,
    store: &str,
    level: BudgetLevel,
    size: u64,
    budget: &StorageBudgetConfig,
    episode: Option<u64>,
) {
    if level == BudgetLevel::Ok {
        return;
    }
    let threshold = if level == BudgetLevel::Hard {
        budget.hard_bytes
    } else {
        budget.warning_bytes
    };
    warnings.push(ActiveStorageWarning {
        warning_sequence: episode.expect("an active budget level has a transition sequence"),
        store: store.to_owned(),
        level,
        size_bytes: size,
        threshold_bytes: threshold,
        message: transition_message(store, level, size, budget),
    });
}

fn growth(previous: &StoragePoint, current: &StoragePoint) -> Option<GrowthPerCompletion> {
    let delta = current
        .completion_count
        .checked_sub(previous.completion_count)
        .filter(|delta| *delta > 0)?;
    Some(GrowthPerCompletion {
        completion_delta: delta,
        data_dir_bytes: rate(previous.data_dir_bytes, current.data_dir_bytes, delta),
        state_dir_bytes: rate(previous.state_dir_bytes, current.state_dir_bytes, delta),
        taskchampion_bytes: rate(
            previous.taskchampion_bytes,
            current.taskchampion_bytes,
            delta,
        ),
        taskchampion_operations: previous
            .taskchampion_operations
            .zip(current.taskchampion_operations)
            .map(|(before, after)| rate(before, after, delta)),
    })
}

fn rate(before: u64, after: u64, delta: u64) -> i64 {
    let difference = i128::from(after) - i128::from(before);
    let value = difference / i128::from(delta);
    value.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

#[derive(Default)]
struct DirectoryUsage {
    allocated: u64,
    apparent: u64,
    files: u64,
}

fn directory_usage(root: &Path) -> Result<DirectoryUsage, StorageError> {
    let mut usage = DirectoryUsage::default();
    let mut pending = vec![root.to_owned()];
    while let Some(path) = pending.pop() {
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => return Err(io_error(&path, source)),
        };
        usage.allocated = usage.allocated.saturating_add(metadata.blocks() * 512);
        usage.apparent = usage.apparent.saturating_add(metadata.len());
        if metadata.file_type().is_dir() {
            let entries = std::fs::read_dir(&path).map_err(|source| io_error(&path, source))?;
            for entry in entries {
                let entry = entry.map_err(|source| io_error(&path, source))?;
                pending.push(entry.path());
            }
        } else {
            usage.files = usage.files.saturating_add(1);
        }
    }
    Ok(usage)
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |metadata| metadata.len())
}

fn taskchampion_metrics(data_dir: &Path) -> TaskchampionMetrics {
    let taskdata = data_dir.join(TASKDATA_DIRECTORY);
    let database = taskdata.join(TASKCHAMPION_DB);
    let database_bytes = file_len(&database);
    let wal_bytes = file_len(&taskdata.join(format!("{TASKCHAMPION_DB}-wal")));
    let shm_bytes = file_len(&taskdata.join(format!("{TASKCHAMPION_DB}-shm")));
    let mut metrics = TaskchampionMetrics {
        database_bytes,
        wal_bytes,
        shm_bytes,
        total_bytes: database_bytes
            .saturating_add(wal_bytes)
            .saturating_add(shm_bytes),
        ..TaskchampionMetrics::default()
    };
    if database_bytes == 0 {
        return metrics;
    }
    let result = (|| -> rusqlite::Result<(u64, u64)> {
        let connection = Connection::open_with_flags(
            &database,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(std::time::Duration::from_millis(250))?;
        let tasks: u64 =
            connection.query_row("SELECT count(*) FROM tasks", [], |row| row.get(0))?;
        let high_water = connection
            .query_row(
                "SELECT seq FROM sqlite_sequence WHERE name = 'operations'",
                [],
                |row| row.get::<_, u64>(0),
            )
            .optional()?
            .unwrap_or(0);
        Ok((tasks, high_water))
    })();
    match result {
        Ok((tasks, high_water)) => {
            metrics.task_count = Some(tasks);
            metrics.operation_high_water = Some(high_water);
        }
        Err(error) => metrics.read_error = Some(error.to_string()),
    }
    metrics
}

fn append_warning(data_dir: &Path, warning: &StorageWarningRecord) -> Result<(), StorageError> {
    let path = data_dir.join(STORAGE_WARNING_FILE);
    let created = !path.exists();
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .map_err(|source| io_error(&path, source))?;
    serde_json::to_writer(&mut file, warning)?;
    file.write_all(b"\n")
        .map_err(|source| io_error(&path, source))?;
    file.sync_all().map_err(|source| io_error(&path, source))?;
    if created {
        File::open(data_dir)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| io_error(data_dir, source))?;
    }
    Ok(())
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), StorageError> {
    let parent = path
        .parent()
        .expect("storage monitor state path always has a parent");
    let temporary = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|source| io_error(&temporary, source))?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")
        .map_err(|source| io_error(&temporary, source))?;
    file.sync_all()
        .map_err(|source| io_error(&temporary, source))?;
    std::fs::rename(&temporary, path).map_err(|source| io_error(path, source))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error(parent, source))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn config(warning: u64, hard: u64) -> StorageConfig {
        StorageConfig {
            poll_interval_sec: 1,
            data_dir: StorageBudgetConfig {
                warning_bytes: warning,
                hard_bytes: hard,
            },
            state_dir: StorageBudgetConfig {
                warning_bytes: warning,
                hard_bytes: hard,
            },
        }
    }

    #[test]
    fn warns_durably_and_refuses_only_at_hard_limit() {
        let root = TempDir::new().unwrap();
        let data = root.path().join("data");
        let state = root.path().join("state");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        let baseline = directory_usage(&data).unwrap().allocated.max(4096);
        let mut monitor =
            StorageMonitor::open(&data, &state, config(baseline + 4096, baseline + 8192), 0)
                .unwrap();
        assert!(monitor.snapshot().unwrap().intake.accepting);

        std::fs::write(data.join("growth"), vec![0_u8; 16 * 1024]).unwrap();
        monitor.refresh(1).unwrap();
        let snapshot = monitor.snapshot().unwrap();
        assert_eq!(snapshot.data_dir.level, BudgetLevel::Hard);
        assert!(!snapshot.intake.accepting);
        assert!(snapshot.growth_per_completion.is_some());
        assert_eq!(
            monitor.take_warnings().last().unwrap().level,
            BudgetLevel::Hard
        );
        let warning_log = std::fs::read_to_string(data.join(STORAGE_WARNING_FILE)).unwrap();
        assert!(warning_log.contains("hard storage budget"));
    }

    #[test]
    fn reads_taskchampion_sizes_counts_and_operation_high_water() {
        let root = TempDir::new().unwrap();
        let taskdata = root.path().join(TASKDATA_DIRECTORY);
        std::fs::create_dir_all(&taskdata).unwrap();
        let database = taskdata.join(TASKCHAMPION_DB);
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE tasks (uuid STRING PRIMARY KEY, data STRING);\n\
                 CREATE TABLE operations (id INTEGER PRIMARY KEY AUTOINCREMENT, data STRING);\n\
                 INSERT INTO tasks VALUES ('one', '{}');\n\
                 INSERT INTO operations (data) VALUES ('one'), ('two');\n\
                 DELETE FROM operations;",
            )
            .unwrap();
        drop(connection);
        std::fs::write(taskdata.join(format!("{TASKCHAMPION_DB}-wal")), b"wal").unwrap();

        let metrics = taskchampion_metrics(root.path());
        assert_eq!(metrics.task_count, Some(1));
        assert_eq!(metrics.operation_high_water, Some(2));
        assert!(metrics.database_bytes > 0);
        assert_eq!(metrics.wal_bytes, 3);
    }
}
