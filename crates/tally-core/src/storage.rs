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
pub const STORAGE_RECOVERY_PERCENT: u64 = 90;
const STORAGE_METRICS_SCHEMA_VERSION: u32 = 2;
const STORAGE_STATE_SCHEMA_VERSION: u32 = 2;
const STORAGE_WARNING_SCHEMA_VERSION: u32 = 2;
const TASKCHAMPION_DB: &str = "taskchampion.sqlite3";

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("storage monitor I/O error at {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("storage monitor JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("storage monitor state error: {0}")]
    State(String),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StoragePressureResource {
    AllocatedBytes,
    FilesystemAvailableBytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoragePressure {
    pub resource: StoragePressureResource,
    pub observed_bytes: u64,
    pub threshold_bytes: u64,
    pub recovery_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreMetrics {
    pub size_bytes: u64,
    pub apparent_bytes: u64,
    pub file_count: u64,
    pub warning_bytes: u64,
    pub hard_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filesystem_available_bytes: Option<u64>,
    pub minimum_free_bytes: u64,
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
    pub pressures: Vec<StoragePressure>,
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

impl StorageMetrics {
    pub fn with_monitor_error(mut self, error: impl Into<String>) -> Self {
        let error = error.into();
        self.monitor_error = Some(error.clone());
        self.intake = unavailable_intake(&error);
        self
    }
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
    pub pressures: Vec<StoragePressure>,
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
    data_dir_pressures: Vec<StoragePressureResource>,
    state_dir_pressures: Vec<StoragePressureResource>,
    next_warning_sequence: u64,
    data_dir_episode: Option<u64>,
    state_dir_episode: Option<u64>,
}

pub struct StorageMonitor {
    data_dir: PathBuf,
    state_dir: PathBuf,
    config: StorageConfig,
    state: Option<PersistentState>,
    snapshot: StorageMetrics,
    pending_warnings: Vec<StorageWarningRecord>,
    notices: Vec<String>,
    next_warning_sequence_floor: u64,
    last_error: Option<String>,
    #[cfg(test)]
    sample_delay: Option<std::time::Duration>,
}

impl StorageMonitor {
    pub fn open(
        data_dir: impl Into<PathBuf>,
        state_dir: impl Into<PathBuf>,
        config: StorageConfig,
        completion_count: u64,
    ) -> Self {
        let data_dir = data_dir.into();
        let state_dir = state_dir.into();
        let state_path = data_dir.join(STORAGE_STATE_FILE);
        let (state, mut notices) = load_persistent_state(&state_path);
        let (next_warning_sequence_floor, warning_notice) =
            warning_sequence_floor(&data_dir.join(STORAGE_WARNING_FILE));
        if let Some(notice) = warning_notice {
            notices.push(notice);
        }
        let snapshot = unavailable_snapshot(
            &config,
            completion_count,
            "storage monitor has not completed its initial sample",
        );
        let mut monitor = Self {
            data_dir,
            state_dir,
            config,
            state,
            snapshot,
            pending_warnings: Vec::new(),
            notices,
            next_warning_sequence_floor,
            last_error: None,
            #[cfg(test)]
            sample_delay: None,
        };
        let _ = monitor.refresh(completion_count);
        monitor
    }

    pub fn poll_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.config.poll_interval_sec)
    }

    pub fn refresh(&mut self, completion_count: u64) -> Result<&StorageMetrics, StorageError> {
        match self.sample(completion_count) {
            Ok(snapshot) => {
                self.last_error = None;
                self.snapshot = snapshot;
                Ok(&self.snapshot)
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
                Err(error)
            }
        }
    }

    pub fn snapshot(&self) -> &StorageMetrics {
        &self.snapshot
    }

    pub fn query_snapshot(&self) -> StorageMetrics {
        match &self.last_error {
            Some(error) => self.snapshot.clone().with_monitor_error(error.clone()),
            None => self.snapshot.clone(),
        }
    }

    pub fn take_warnings(&mut self) -> Vec<StorageWarningRecord> {
        std::mem::take(&mut self.pending_warnings)
    }

    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    #[cfg(test)]
    pub fn set_sample_delay(&mut self, delay: std::time::Duration) {
        self.sample_delay = Some(delay);
    }

    fn sample(&mut self, completion_count: u64) -> Result<StorageMetrics, StorageError> {
        #[cfg(test)]
        if let Some(delay) = self.sample_delay {
            std::thread::sleep(delay);
        }

        let sampled_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let data_usage = directory_usage(&self.data_dir)?;
        let state_usage = directory_usage(&self.state_dir)?;
        let data_available = filesystem_available(&self.data_dir)?;
        let state_available = filesystem_available(&self.state_dir)?;
        let taskchampion = taskchampion_metrics(&self.data_dir);
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
            old_data_pressures,
            old_state_pressures,
            next_warning_sequence,
            mut data_episode,
            mut state_episode,
        ) = self.state.as_ref().map_or_else(
            || {
                (
                    None,
                    BudgetLevel::Ok,
                    BudgetLevel::Ok,
                    Vec::new(),
                    Vec::new(),
                    self.next_warning_sequence_floor,
                    None,
                    None,
                )
            },
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
                    state.data_dir_pressures.clone(),
                    state.state_dir_pressures.clone(),
                    state
                        .next_warning_sequence
                        .max(self.next_warning_sequence_floor),
                    state.data_dir_episode,
                    state.state_dir_episode,
                )
            },
        );

        let data_decision = budget_decision(
            data_usage.allocated,
            data_available,
            &self.config.data_dir,
            old_data_level,
            &old_data_pressures,
        );
        let state_decision = budget_decision(
            state_usage.allocated,
            state_available,
            &self.config.state_dir,
            old_state_level,
            &old_state_pressures,
        );
        let mut next_sequence = next_warning_sequence;
        record_transition(
            &self.data_dir,
            &mut self.pending_warnings,
            &sampled_at,
            "dataDir",
            old_data_level,
            &data_decision,
            data_usage.allocated,
            data_available,
            &self.config.data_dir,
            &mut data_episode,
            &mut next_sequence,
        )?;
        record_transition(
            &self.data_dir,
            &mut self.pending_warnings,
            &sampled_at,
            "stateDir",
            old_state_level,
            &state_decision,
            state_usage.allocated,
            state_available,
            &self.config.state_dir,
            &mut state_episode,
            &mut next_sequence,
        )?;

        let persistent = PersistentState {
            schema_version: STORAGE_STATE_SCHEMA_VERSION,
            previous: previous.clone(),
            current: point.clone(),
            data_dir_level: data_decision.level,
            state_dir_level: state_decision.level,
            data_dir_pressures: pressure_resources(&data_decision),
            state_dir_pressures: pressure_resources(&state_decision),
            next_warning_sequence: next_sequence,
            data_dir_episode: data_episode,
            state_dir_episode: state_episode,
        };
        write_json_atomic(&self.data_dir.join(STORAGE_STATE_FILE), &persistent)?;
        self.state = Some(persistent);
        self.next_warning_sequence_floor = next_sequence;

        let mut active_warnings = Vec::new();
        push_active_warning(
            &mut active_warnings,
            "dataDir",
            &data_decision,
            data_usage.allocated,
            data_available,
            &self.config.data_dir,
            data_episode,
        )?;
        push_active_warning(
            &mut active_warnings,
            "stateDir",
            &state_decision,
            state_usage.allocated,
            state_available,
            &self.config.state_dir,
            state_episode,
        )?;
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
            schema_version: STORAGE_METRICS_SCHEMA_VERSION,
            sampled_at,
            completion_count,
            intake,
            data_dir: store_metrics(
                &data_usage,
                data_available,
                &self.config.data_dir,
                data_decision.level,
            ),
            state_dir: store_metrics(
                &state_usage,
                state_available,
                &self.config.state_dir,
                state_decision.level,
            ),
            taskchampion,
            growth_per_completion: previous
                .as_ref()
                .and_then(|previous| growth(previous, &point)),
            active_warnings,
            monitor_error: None,
        })
    }
}

#[derive(Debug)]
struct BudgetDecision {
    level: BudgetLevel,
    pressures: Vec<StoragePressure>,
}

fn budget_decision(
    size: u64,
    available: u64,
    budget: &StorageBudgetConfig,
    previous_level: BudgetLevel,
    previous_pressures: &[StoragePressureResource],
) -> BudgetDecision {
    let hard_size_recovery = lower_recovery(budget.hard_bytes);
    let free_recovery = upper_recovery(budget.minimum_free_bytes);
    let previous_size_pressure =
        previous_pressures.contains(&StoragePressureResource::AllocatedBytes);
    let previous_free_pressure =
        previous_pressures.contains(&StoragePressureResource::FilesystemAvailableBytes);
    let hard_size = size >= budget.hard_bytes
        || (previous_level == BudgetLevel::Hard
            && previous_size_pressure
            && size >= hard_size_recovery);
    let hard_free = available < budget.minimum_free_bytes
        || (previous_level == BudgetLevel::Hard
            && previous_free_pressure
            && available < free_recovery);
    if hard_size || hard_free {
        let mut pressures = Vec::new();
        if hard_size {
            pressures.push(StoragePressure {
                resource: StoragePressureResource::AllocatedBytes,
                observed_bytes: size,
                threshold_bytes: budget.hard_bytes,
                recovery_bytes: hard_size_recovery,
            });
        }
        if hard_free {
            pressures.push(StoragePressure {
                resource: StoragePressureResource::FilesystemAvailableBytes,
                observed_bytes: available,
                threshold_bytes: budget.minimum_free_bytes,
                recovery_bytes: free_recovery,
            });
        }
        return BudgetDecision {
            level: BudgetLevel::Hard,
            pressures,
        };
    }

    let warning_recovery = lower_recovery(budget.warning_bytes);
    let warning = size >= budget.warning_bytes
        || (previous_level != BudgetLevel::Ok
            && previous_size_pressure
            && size >= warning_recovery);
    if warning {
        return BudgetDecision {
            level: BudgetLevel::Warning,
            pressures: vec![StoragePressure {
                resource: StoragePressureResource::AllocatedBytes,
                observed_bytes: size,
                threshold_bytes: budget.warning_bytes,
                recovery_bytes: warning_recovery,
            }],
        };
    }

    BudgetDecision {
        level: BudgetLevel::Ok,
        pressures: Vec::new(),
    }
}

fn lower_recovery(threshold: u64) -> u64 {
    ((u128::from(threshold) * u128::from(STORAGE_RECOVERY_PERCENT)) / 100).min(u128::from(u64::MAX))
        as u64
}

fn upper_recovery(threshold: u64) -> u64 {
    let numerator = u128::from(threshold) * 100;
    ((numerator + u128::from(STORAGE_RECOVERY_PERCENT - 1)) / u128::from(STORAGE_RECOVERY_PERCENT))
        .min(u128::from(u64::MAX)) as u64
}

fn pressure_resources(decision: &BudgetDecision) -> Vec<StoragePressureResource> {
    decision
        .pressures
        .iter()
        .map(|pressure| pressure.resource)
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn record_transition(
    data_dir: &Path,
    pending: &mut Vec<StorageWarningRecord>,
    sampled_at: &str,
    store: &str,
    previous_level: BudgetLevel,
    decision: &BudgetDecision,
    size: u64,
    available: u64,
    budget: &StorageBudgetConfig,
    episode: &mut Option<u64>,
    next_sequence: &mut u64,
) -> Result<(), StorageError> {
    let repair_missing_episode = decision.level != BudgetLevel::Ok && episode.is_none();
    if previous_level == decision.level && !repair_missing_episode {
        return Ok(());
    }
    let sequence = *next_sequence;
    let threshold = match decision.level {
        BudgetLevel::Ok | BudgetLevel::Warning => budget.warning_bytes,
        BudgetLevel::Hard => budget.hard_bytes,
    };
    let message = transition_message(store, decision, size, available, budget);
    let record = StorageWarningRecord {
        schema_version: STORAGE_WARNING_SCHEMA_VERSION,
        sequence,
        recorded_at: sampled_at.to_owned(),
        store: store.to_owned(),
        previous_level,
        level: decision.level,
        size_bytes: size,
        threshold_bytes: threshold,
        pressures: decision.pressures.clone(),
        message,
    };
    append_warning(data_dir, &record)?;
    pending.push(record);
    if decision.level == BudgetLevel::Ok {
        *episode = None;
    } else if previous_level == BudgetLevel::Ok || episode.is_none() {
        *episode = Some(sequence);
    }
    *next_sequence = next_sequence.saturating_add(1);
    Ok(())
}

fn transition_message(
    store: &str,
    decision: &BudgetDecision,
    size: u64,
    available: u64,
    budget: &StorageBudgetConfig,
) -> String {
    match decision.level {
        BudgetLevel::Ok => format!(
            "tally {store} storage pressure recovered: allocated {size} bytes; filesystem available {available} bytes"
        ),
        BudgetLevel::Warning => format!(
            "tally {store} allocated-size warning is active: {size} bytes (warning {}, recovers below {})",
            budget.warning_bytes,
            lower_recovery(budget.warning_bytes)
        ),
        BudgetLevel::Hard => {
            let reasons = decision
                .pressures
                .iter()
                .map(|pressure| match pressure.resource {
                    StoragePressureResource::AllocatedBytes => format!(
                        "allocated {} bytes (hard {}, recovers below {})",
                        pressure.observed_bytes,
                        pressure.threshold_bytes,
                        pressure.recovery_bytes
                    ),
                    StoragePressureResource::FilesystemAvailableBytes => format!(
                        "filesystem available {} bytes (minimum {}, recovers at {})",
                        pressure.observed_bytes,
                        pressure.threshold_bytes,
                        pressure.recovery_bytes
                    ),
                })
                .collect::<Vec<_>>();
            format!(
                "tally {store} hard storage protection is active: {}",
                reasons.join("; ")
            )
        }
    }
}

fn push_active_warning(
    warnings: &mut Vec<ActiveStorageWarning>,
    store: &str,
    decision: &BudgetDecision,
    size: u64,
    available: u64,
    budget: &StorageBudgetConfig,
    episode: Option<u64>,
) -> Result<(), StorageError> {
    if decision.level == BudgetLevel::Ok {
        return Ok(());
    }
    let warning_sequence = episode.ok_or_else(|| {
        StorageError::State(format!(
            "active {store} pressure has no durable warning episode"
        ))
    })?;
    let threshold = if decision.level == BudgetLevel::Hard {
        budget.hard_bytes
    } else {
        budget.warning_bytes
    };
    warnings.push(ActiveStorageWarning {
        warning_sequence,
        store: store.to_owned(),
        level: decision.level,
        size_bytes: size,
        threshold_bytes: threshold,
        pressures: decision.pressures.clone(),
        message: transition_message(store, decision, size, available, budget),
    });
    Ok(())
}

fn store_metrics(
    usage: &DirectoryUsage,
    available: u64,
    budget: &StorageBudgetConfig,
    level: BudgetLevel,
) -> StoreMetrics {
    StoreMetrics {
        size_bytes: usage.allocated,
        apparent_bytes: usage.apparent,
        file_count: usage.files,
        warning_bytes: budget.warning_bytes,
        hard_bytes: budget.hard_bytes,
        filesystem_available_bytes: Some(available),
        minimum_free_bytes: budget.minimum_free_bytes,
        level,
    }
}

fn unavailable_snapshot(
    config: &StorageConfig,
    completion_count: u64,
    error: &str,
) -> StorageMetrics {
    let empty_store = |budget: &StorageBudgetConfig| StoreMetrics {
        size_bytes: 0,
        apparent_bytes: 0,
        file_count: 0,
        warning_bytes: budget.warning_bytes,
        hard_bytes: budget.hard_bytes,
        filesystem_available_bytes: None,
        minimum_free_bytes: budget.minimum_free_bytes,
        level: BudgetLevel::Ok,
    };
    StorageMetrics {
        schema_version: STORAGE_METRICS_SCHEMA_VERSION,
        sampled_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        completion_count,
        intake: unavailable_intake(error),
        data_dir: empty_store(&config.data_dir),
        state_dir: empty_store(&config.state_dir),
        taskchampion: TaskchampionMetrics::default(),
        growth_per_completion: None,
        active_warnings: Vec::new(),
        monitor_error: Some(error.to_owned()),
    }
}

fn unavailable_intake(error: &str) -> IntakeStatus {
    IntakeStatus {
        accepting: false,
        reason: Some(format!(
            "storage monitor is unavailable: {error}; new intake is refused while already-admitted work continues"
        )),
    }
}

fn load_persistent_state(path: &Path) -> (Option<PersistentState>, Vec<String>) {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return (None, Vec::new()),
        Err(error) => {
            return (
                None,
                vec![format!(
                    "ignored unreadable derived storage state {}: {error}; a fresh sample will replace it",
                    path.display()
                )],
            )
        }
    };
    match decode_persistent_state(&bytes) {
        Ok(state) => (Some(state), Vec::new()),
        Err(error) => (
            None,
            vec![format!(
                "ignored incompatible derived storage state {}: {error}; a fresh sample will replace it",
                path.display()
            )],
        ),
    }
}

fn warning_sequence_floor(path: &Path) -> (u64, Option<String>) {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return (1, None),
        Err(error) => {
            return (
                1,
                Some(format!(
                    "could not read durable storage warning high-water {}: {error}; warning sequence continuity could not be recovered",
                    path.display()
                )),
            )
        }
    };
    let mut high_water = 0_u64;
    let mut invalid = 0_u64;
    for line in contents.lines().filter(|line| !line.trim().is_empty()) {
        match serde_json::from_str::<serde_json::Value>(line)
            .ok()
            .and_then(|value| value.get("sequence").and_then(serde_json::Value::as_u64))
        {
            Some(sequence) => high_water = high_water.max(sequence),
            None => invalid = invalid.saturating_add(1),
        }
    }
    let notice = (invalid > 0).then(|| {
        format!(
            "ignored {invalid} malformed records while recovering the storage warning high-water from {}",
            path.display()
        )
    });
    (high_water.saturating_add(1).max(1), notice)
}

fn decode_persistent_state(bytes: &[u8]) -> Result<PersistentState, String> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    let schema_version = value
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "schemaVersion is missing or is not an unsigned integer".to_owned())?;
    if schema_version != u64::from(STORAGE_STATE_SCHEMA_VERSION) {
        return Err(format!(
            "unsupported schemaVersion {schema_version}; expected {STORAGE_STATE_SCHEMA_VERSION}"
        ));
    }
    let state: PersistentState =
        serde_json::from_value(value).map_err(|error| error.to_string())?;
    validate_persistent_state(&state)?;
    Ok(state)
}

fn validate_persistent_state(state: &PersistentState) -> Result<(), String> {
    if state.next_warning_sequence == 0 {
        return Err("nextWarningSequence must be positive".to_owned());
    }
    for (store, level, pressures, episode) in [
        (
            "dataDir",
            state.data_dir_level,
            &state.data_dir_pressures,
            state.data_dir_episode,
        ),
        (
            "stateDir",
            state.state_dir_level,
            &state.state_dir_pressures,
            state.state_dir_episode,
        ),
    ] {
        if level == BudgetLevel::Ok {
            if !pressures.is_empty() || episode.is_some() {
                return Err(format!(
                    "{store} has ok level with an active pressure or episode"
                ));
            }
        } else if pressures.is_empty()
            || episode.is_none()
            || episode
                .is_some_and(|sequence| sequence == 0 || sequence >= state.next_warning_sequence)
        {
            return Err(format!(
                "{store} has an active level without a valid pressure episode"
            ));
        }
    }
    Ok(())
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
        usage.allocated = usage
            .allocated
            .saturating_add(metadata.blocks().saturating_mul(512));
        usage.apparent = usage.apparent.saturating_add(metadata.len());
        if metadata.file_type().is_dir() {
            let entries = match std::fs::read_dir(&path) {
                Ok(entries) => entries,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => return Err(io_error(&path, source)),
            };
            for entry in entries {
                match entry {
                    Ok(entry) => pending.push(entry.path()),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(source) => return Err(io_error(&path, source)),
                }
            }
        } else {
            usage.files = usage.files.saturating_add(1);
        }
    }
    Ok(usage)
}

fn filesystem_available(path: &Path) -> Result<u64, StorageError> {
    fs2::available_space(path).map_err(|source| io_error(path, source))
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
        .ok_or_else(|| StorageError::State("storage state path has no parent".to_owned()))?;
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
                minimum_free_bytes: 1,
            },
            state_dir: StorageBudgetConfig {
                warning_bytes: warning,
                hard_bytes: hard,
                minimum_free_bytes: 1,
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
            StorageMonitor::open(&data, &state, config(baseline + 4096, baseline + 8192), 0);
        assert!(monitor.snapshot().intake.accepting);

        std::fs::write(data.join("growth"), vec![0_u8; 16 * 1024]).unwrap();
        monitor.refresh(1).unwrap();
        let snapshot = monitor.snapshot();
        assert_eq!(snapshot.data_dir.level, BudgetLevel::Hard);
        assert!(!snapshot.intake.accepting);
        assert!(snapshot.growth_per_completion.is_some());
        assert_eq!(
            monitor.take_warnings().last().unwrap().level,
            BudgetLevel::Hard
        );
        let warning_log = std::fs::read_to_string(data.join(STORAGE_WARNING_FILE)).unwrap();
        assert!(warning_log.contains("hard storage protection"));
    }

    #[test]
    fn free_space_floor_is_hard_and_hysteretic() {
        let budget = StorageBudgetConfig {
            warning_bytes: 1_000,
            hard_bytes: 2_000,
            minimum_free_bytes: 900,
        };
        let hard = budget_decision(10, 899, &budget, BudgetLevel::Ok, &[]);
        assert_eq!(hard.level, BudgetLevel::Hard);
        assert_eq!(
            hard.pressures[0].resource,
            StoragePressureResource::FilesystemAvailableBytes
        );

        let still_hard = budget_decision(
            10,
            950,
            &budget,
            BudgetLevel::Hard,
            &[StoragePressureResource::FilesystemAvailableBytes],
        );
        assert_eq!(still_hard.level, BudgetLevel::Hard);
        let recovered = budget_decision(
            10,
            1_000,
            &budget,
            BudgetLevel::Hard,
            &[StoragePressureResource::FilesystemAvailableBytes],
        );
        assert_eq!(recovered.level, BudgetLevel::Ok);
    }

    #[test]
    fn size_thresholds_do_not_flap_inside_recovery_band() {
        let budget = StorageBudgetConfig {
            warning_bytes: 1_000,
            hard_bytes: 2_000,
            minimum_free_bytes: 1,
        };
        let hard = budget_decision(2_000, 10_000, &budget, BudgetLevel::Ok, &[]);
        assert_eq!(hard.level, BudgetLevel::Hard);
        let still_hard = budget_decision(
            1_900,
            10_000,
            &budget,
            BudgetLevel::Hard,
            &[StoragePressureResource::AllocatedBytes],
        );
        assert_eq!(still_hard.level, BudgetLevel::Hard);
        let warning = budget_decision(
            1_799,
            10_000,
            &budget,
            BudgetLevel::Hard,
            &[StoragePressureResource::AllocatedBytes],
        );
        assert_eq!(warning.level, BudgetLevel::Warning);
        let still_warning = budget_decision(
            950,
            10_000,
            &budget,
            BudgetLevel::Warning,
            &[StoragePressureResource::AllocatedBytes],
        );
        assert_eq!(still_warning.level, BudgetLevel::Warning);
        let recovered = budget_decision(
            899,
            10_000,
            &budget,
            BudgetLevel::Warning,
            &[StoragePressureResource::AllocatedBytes],
        );
        assert_eq!(recovered.level, BudgetLevel::Ok);
    }

    #[test]
    fn severity_changes_share_one_campaign_episode_until_full_recovery() {
        let root = TempDir::new().unwrap();
        let budget = StorageBudgetConfig {
            warning_bytes: 1_000,
            hard_bytes: 2_000,
            minimum_free_bytes: 1,
        };
        let warning = budget_decision(1_000, 10_000, &budget, BudgetLevel::Ok, &[]);
        let hard = budget_decision(
            2_000,
            10_000,
            &budget,
            BudgetLevel::Warning,
            &[StoragePressureResource::AllocatedBytes],
        );
        let ok = budget_decision(
            100,
            10_000,
            &budget,
            BudgetLevel::Warning,
            &[StoragePressureResource::AllocatedBytes],
        );
        let mut pending = Vec::new();
        let mut episode = None;
        let mut next = 1;
        record_transition(
            root.path(),
            &mut pending,
            "now",
            "dataDir",
            BudgetLevel::Ok,
            &warning,
            1_000,
            10_000,
            &budget,
            &mut episode,
            &mut next,
        )
        .unwrap();
        assert_eq!(episode, Some(1));
        record_transition(
            root.path(),
            &mut pending,
            "now",
            "dataDir",
            BudgetLevel::Warning,
            &hard,
            2_000,
            10_000,
            &budget,
            &mut episode,
            &mut next,
        )
        .unwrap();
        assert_eq!(episode, Some(1));
        record_transition(
            root.path(),
            &mut pending,
            "now",
            "dataDir",
            BudgetLevel::Hard,
            &ok,
            100,
            10_000,
            &budget,
            &mut episode,
            &mut next,
        )
        .unwrap();
        assert_eq!(episode, None);
        record_transition(
            root.path(),
            &mut pending,
            "now",
            "dataDir",
            BudgetLevel::Ok,
            &warning,
            1_000,
            10_000,
            &budget,
            &mut episode,
            &mut next,
        )
        .unwrap();
        assert_eq!(episode, Some(4));
    }

    #[test]
    fn incompatible_advisory_state_resets_instead_of_blocking_startup() {
        let root = TempDir::new().unwrap();
        let data = root.path().join("data");
        let state = root.path().join("state");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        for invalid in [
            b"not json".as_slice(),
            br#"{"schemaVersion":999,"foreign":true}"#.as_slice(),
            br#"{"schemaVersion":2,"foreign":true}"#.as_slice(),
        ] {
            std::fs::write(data.join(STORAGE_STATE_FILE), invalid).unwrap();
            let mut monitor =
                StorageMonitor::open(&data, &state, config(u64::MAX - 1, u64::MAX), 0);
            assert!(monitor.snapshot().intake.accepting);
            assert_eq!(monitor.take_notices().len(), 1);
            let replaced: serde_json::Value =
                serde_json::from_slice(&std::fs::read(data.join(STORAGE_STATE_FILE)).unwrap())
                    .unwrap();
            assert_eq!(replaced["schemaVersion"], STORAGE_STATE_SCHEMA_VERSION);
        }
    }

    #[test]
    fn reset_state_recovers_warning_sequence_from_the_durable_log() {
        let root = TempDir::new().unwrap();
        let data = root.path().join("data");
        let state = root.path().join("state");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(
            data.join(STORAGE_WARNING_FILE),
            r#"{"schemaVersion":1,"sequence":42}
"#,
        )
        .unwrap();
        std::fs::write(data.join(STORAGE_STATE_FILE), b"corrupt").unwrap();
        let baseline = directory_usage(&data).unwrap().allocated;
        let mut monitor = StorageMonitor::open(
            &data,
            &state,
            config(baseline.saturating_sub(1).max(1), baseline.max(2)),
            0,
        );
        let warnings = monitor.take_warnings();
        assert_eq!(warnings.first().unwrap().sequence, 43);
    }

    #[test]
    fn disappearing_directory_is_skipped_during_walk() {
        let root = TempDir::new().unwrap();
        let vanished = root.path().join("vanished");
        assert_eq!(directory_usage(&vanished).unwrap().allocated, 0);
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
