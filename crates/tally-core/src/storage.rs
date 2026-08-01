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
pub const STORAGE_FREE_RECOVERY_MIN_BYTES: u64 = 1024 * 1024 * 1024;
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
    pub warning_free_bytes: u64,
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
    pub free_space_checked_at: String,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    sample_error: Option<String>,
    free_space_error: Option<String>,
    #[cfg(test)]
    sample_delay: Option<std::time::Duration>,
    #[cfg(test)]
    sample_panic_once: bool,
    #[cfg(test)]
    free_space_override: Option<(u64, u64)>,
}

pub(crate) struct StorageSampleRequest {
    data_dir: PathBuf,
    state_dir: PathBuf,
    completion_count: u64,
    #[cfg(test)]
    delay: Option<std::time::Duration>,
    #[cfg(test)]
    panic: bool,
}

pub(crate) struct StorageMeasurement {
    sampled_at: String,
    completion_count: u64,
    data_usage: DirectoryUsage,
    state_usage: DirectoryUsage,
    data_available: u64,
    state_available: u64,
    taskchampion: TaskchampionMetrics,
}

struct PressureEvaluation {
    data_decision: BudgetDecision,
    state_decision: BudgetDecision,
    data_episode: Option<u64>,
    state_episode: Option<u64>,
}

impl StorageSampleRequest {
    pub(crate) fn run(self) -> Result<StorageMeasurement, StorageError> {
        #[cfg(test)]
        if self.panic {
            panic!("injected storage sampler panic");
        }
        #[cfg(test)]
        if let Some(delay) = self.delay {
            std::thread::sleep(delay);
        }

        let sampled_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let data_usage = directory_usage(&self.data_dir)?;
        let state_usage = directory_usage(&self.state_dir)?;
        let data_available = filesystem_available(&self.data_dir)?;
        let state_available = filesystem_available(&self.state_dir)?;
        let taskchampion = taskchampion_metrics(&self.data_dir);
        Ok(StorageMeasurement {
            sampled_at,
            completion_count: self.completion_count,
            data_usage,
            state_usage,
            data_available,
            state_available,
            taskchampion,
        })
    }
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
            sample_error: None,
            free_space_error: None,
            #[cfg(test)]
            sample_delay: None,
            #[cfg(test)]
            sample_panic_once: false,
            #[cfg(test)]
            free_space_override: None,
        };
        let _ = monitor.refresh(completion_count);
        monitor
    }

    pub fn poll_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.config.poll_interval_sec)
    }

    pub(crate) fn sample_request(&mut self, completion_count: u64) -> StorageSampleRequest {
        StorageSampleRequest {
            data_dir: self.data_dir.clone(),
            state_dir: self.state_dir.clone(),
            completion_count,
            #[cfg(test)]
            delay: self.sample_delay,
            #[cfg(test)]
            panic: std::mem::take(&mut self.sample_panic_once),
        }
    }

    pub fn refresh(&mut self, completion_count: u64) -> Result<&StorageMetrics, StorageError> {
        let measurement = self.sample_request(completion_count).run();
        self.apply_measurement_result(measurement)
    }

    pub(crate) fn apply_measurement_result(
        &mut self,
        measurement: Result<StorageMeasurement, StorageError>,
    ) -> Result<&StorageMetrics, StorageError> {
        match measurement.and_then(|measurement| self.apply_measurement(measurement)) {
            Ok(()) => {
                self.sample_error = None;
                self.free_space_error = None;
                Ok(&self.snapshot)
            }
            Err(error) => {
                self.sample_error = Some(error.to_string());
                Err(error)
            }
        }
    }

    pub(crate) fn record_sample_worker_failure(&mut self, error: impl Into<String>) {
        self.sample_error = Some(error.into());
    }

    pub(crate) fn refresh_free_space(&mut self) -> Result<&StorageMetrics, StorageError> {
        let checked_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let available = self.measure_free_space();
        match available.and_then(|(data_available, state_available)| {
            if self.sample_error.is_some() {
                self.snapshot.free_space_checked_at = checked_at;
                self.snapshot.data_dir.filesystem_available_bytes = Some(data_available);
                self.snapshot.state_dir.filesystem_available_bytes = Some(state_available);
                return Ok(());
            }
            self.apply_free_space(checked_at, data_available, state_available)
        }) {
            Ok(()) => {
                self.free_space_error = None;
                Ok(&self.snapshot)
            }
            Err(error) => {
                self.free_space_error = Some(error.to_string());
                Err(error)
            }
        }
    }

    pub fn snapshot(&self) -> &StorageMetrics {
        &self.snapshot
    }

    pub fn query_snapshot(&self) -> StorageMetrics {
        let errors = [
            self.sample_error.as_deref(),
            self.free_space_error.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        if errors.is_empty() {
            self.snapshot.clone()
        } else {
            self.snapshot.clone().with_monitor_error(errors.join("; "))
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

    #[cfg(test)]
    pub fn set_sample_panic_once(&mut self) {
        self.sample_panic_once = true;
    }

    #[cfg(test)]
    pub fn set_free_space_override(&mut self, data_available: u64, state_available: u64) {
        self.free_space_override = Some((data_available, state_available));
    }

    fn measure_free_space(&self) -> Result<(u64, u64), StorageError> {
        #[cfg(test)]
        if let Some(available) = self.free_space_override {
            return Ok(available);
        }
        Ok((
            filesystem_available(&self.data_dir)?,
            filesystem_available(&self.state_dir)?,
        ))
    }

    fn apply_measurement(&mut self, measurement: StorageMeasurement) -> Result<(), StorageError> {
        let StorageMeasurement {
            sampled_at,
            completion_count,
            data_usage,
            state_usage,
            data_available,
            state_available,
            taskchampion,
        } = measurement;
        let point = StoragePoint {
            sampled_at: sampled_at.clone(),
            completion_count,
            data_dir_bytes: data_usage.allocated,
            state_dir_bytes: state_usage.allocated,
            taskchampion_bytes: taskchampion.total_bytes,
            taskchampion_operations: taskchampion.operation_high_water,
        };

        let previous = self.state.as_ref().and_then(|state| {
            if completion_count > state.current.completion_count {
                Some(state.current.clone())
            } else if completion_count == state.current.completion_count {
                state.previous.clone()
            } else {
                None
            }
        });
        let evaluation = self.evaluate_and_persist(
            &sampled_at,
            previous.clone(),
            point.clone(),
            data_usage.allocated,
            state_usage.allocated,
            data_available,
            state_available,
            true,
        )?;
        let (active_warnings, intake) = active_warnings_and_intake(
            &evaluation,
            data_usage.allocated,
            state_usage.allocated,
            data_available,
            state_available,
            &self.config,
        )?;

        self.snapshot = StorageMetrics {
            schema_version: STORAGE_METRICS_SCHEMA_VERSION,
            sampled_at: sampled_at.clone(),
            free_space_checked_at: sampled_at,
            completion_count,
            intake,
            data_dir: store_metrics(
                &data_usage,
                data_available,
                &self.config.data_dir,
                evaluation.data_decision.level,
            ),
            state_dir: store_metrics(
                &state_usage,
                state_available,
                &self.config.state_dir,
                evaluation.state_decision.level,
            ),
            taskchampion,
            growth_per_completion: previous
                .as_ref()
                .and_then(|previous| growth(previous, &point)),
            active_warnings,
            monitor_error: None,
        };
        Ok(())
    }

    fn apply_free_space(
        &mut self,
        checked_at: String,
        data_available: u64,
        state_available: u64,
    ) -> Result<(), StorageError> {
        let state = self.state.as_ref().ok_or_else(|| {
            StorageError::State("free-space check has no completed tree sample".to_owned())
        })?;
        let evaluation = self.evaluate_and_persist(
            &checked_at,
            state.previous.clone(),
            state.current.clone(),
            self.snapshot.data_dir.size_bytes,
            self.snapshot.state_dir.size_bytes,
            data_available,
            state_available,
            false,
        )?;
        let (active_warnings, intake) = active_warnings_and_intake(
            &evaluation,
            self.snapshot.data_dir.size_bytes,
            self.snapshot.state_dir.size_bytes,
            data_available,
            state_available,
            &self.config,
        )?;
        self.snapshot.free_space_checked_at = checked_at;
        self.snapshot.intake = intake;
        self.snapshot.data_dir.filesystem_available_bytes = Some(data_available);
        self.snapshot.data_dir.level = evaluation.data_decision.level;
        self.snapshot.state_dir.filesystem_available_bytes = Some(state_available);
        self.snapshot.state_dir.level = evaluation.state_decision.level;
        self.snapshot.active_warnings = active_warnings;
        self.snapshot.monitor_error = None;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn evaluate_and_persist(
        &mut self,
        observed_at: &str,
        previous: Option<StoragePoint>,
        current: StoragePoint,
        data_size: u64,
        state_size: u64,
        data_available: u64,
        state_available: u64,
        persist_always: bool,
    ) -> Result<PressureEvaluation, StorageError> {
        let (
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
                (
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
            data_size,
            data_available,
            &self.config.data_dir,
            old_data_level,
            &old_data_pressures,
        );
        let state_decision = budget_decision(
            state_size,
            state_available,
            &self.config.state_dir,
            old_state_level,
            &old_state_pressures,
        );
        let mut next_sequence = next_warning_sequence;
        record_transition(
            &self.data_dir,
            &mut self.pending_warnings,
            observed_at,
            "dataDir",
            old_data_level,
            &data_decision,
            data_size,
            data_available,
            &self.config.data_dir,
            &mut data_episode,
            &mut next_sequence,
        )?;
        record_transition(
            &self.data_dir,
            &mut self.pending_warnings,
            observed_at,
            "stateDir",
            old_state_level,
            &state_decision,
            state_size,
            state_available,
            &self.config.state_dir,
            &mut state_episode,
            &mut next_sequence,
        )?;
        let persistent = PersistentState {
            schema_version: STORAGE_STATE_SCHEMA_VERSION,
            previous,
            current,
            data_dir_level: data_decision.level,
            state_dir_level: state_decision.level,
            data_dir_pressures: pressure_resources(&data_decision),
            state_dir_pressures: pressure_resources(&state_decision),
            next_warning_sequence: next_sequence,
            data_dir_episode: data_episode,
            state_dir_episode: state_episode,
        };
        if persist_always || self.state.as_ref() != Some(&persistent) {
            write_json_atomic(&self.data_dir.join(STORAGE_STATE_FILE), &persistent)?;
        }
        self.state = Some(persistent);
        self.next_warning_sequence_floor = next_sequence;
        Ok(PressureEvaluation {
            data_decision,
            state_decision,
            data_episode,
            state_episode,
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
    let hard_free_recovery = free_recovery(budget.minimum_free_bytes);
    let warning_size_recovery = lower_recovery(budget.warning_bytes);
    let warning_free_recovery = free_recovery(budget.warning_free_bytes);
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
            && available < hard_free_recovery);
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
                recovery_bytes: hard_free_recovery,
            });
        }
        return BudgetDecision {
            level: BudgetLevel::Hard,
            pressures,
        };
    }

    let warning_size = size >= budget.warning_bytes
        || (previous_level != BudgetLevel::Ok
            && previous_size_pressure
            && size >= warning_size_recovery);
    let warning_free = available < budget.warning_free_bytes
        || (previous_level != BudgetLevel::Ok
            && previous_free_pressure
            && available < warning_free_recovery);
    if warning_size || warning_free {
        let mut pressures = Vec::new();
        if warning_size {
            pressures.push(StoragePressure {
                resource: StoragePressureResource::AllocatedBytes,
                observed_bytes: size,
                threshold_bytes: budget.warning_bytes,
                recovery_bytes: warning_size_recovery,
            });
        }
        if warning_free {
            pressures.push(StoragePressure {
                resource: StoragePressureResource::FilesystemAvailableBytes,
                observed_bytes: available,
                threshold_bytes: budget.warning_free_bytes,
                recovery_bytes: warning_free_recovery,
            });
        }
        return BudgetDecision {
            level: BudgetLevel::Warning,
            pressures,
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

fn free_recovery(threshold: u64) -> u64 {
    let proportional = threshold.div_ceil(10);
    threshold.saturating_add(proportional.max(STORAGE_FREE_RECOVERY_MIN_BYTES))
}

fn pressure_resources(decision: &BudgetDecision) -> Vec<StoragePressureResource> {
    decision
        .pressures
        .iter()
        .map(|pressure| pressure.resource)
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn active_warnings_and_intake(
    evaluation: &PressureEvaluation,
    data_size: u64,
    state_size: u64,
    data_available: u64,
    state_available: u64,
    config: &StorageConfig,
) -> Result<(Vec<ActiveStorageWarning>, IntakeStatus), StorageError> {
    let mut active_warnings = Vec::new();
    push_active_warning(
        &mut active_warnings,
        "dataDir",
        &evaluation.data_decision,
        data_size,
        data_available,
        &config.data_dir,
        evaluation.data_episode,
    )?;
    push_active_warning(
        &mut active_warnings,
        "stateDir",
        &evaluation.state_decision,
        state_size,
        state_available,
        &config.state_dir,
        evaluation.state_episode,
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
    Ok((active_warnings, intake))
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
    let threshold = decision
        .pressures
        .first()
        .map_or_else(|| budget.warning_bytes, |pressure| pressure.threshold_bytes);
    let message = transition_message(store, decision, size, available);
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

fn transition_message(store: &str, decision: &BudgetDecision, size: u64, available: u64) -> String {
    match decision.level {
        BudgetLevel::Ok => format!(
            "tally {store} storage pressure recovered: allocated {size} bytes; filesystem available {available} bytes"
        ),
        BudgetLevel::Warning => {
            let reasons = decision
                .pressures
                .iter()
                .map(|pressure| match pressure.resource {
                    StoragePressureResource::AllocatedBytes => format!(
                        "allocated {} bytes (warning {}, recovers below {})",
                        pressure.observed_bytes,
                        pressure.threshold_bytes,
                        pressure.recovery_bytes
                    ),
                    StoragePressureResource::FilesystemAvailableBytes => format!(
                        "filesystem available {} bytes (warning below {}, recovers at {})",
                        pressure.observed_bytes,
                        pressure.threshold_bytes,
                        pressure.recovery_bytes
                    ),
                })
                .collect::<Vec<_>>();
            format!(
                "tally {store} storage warning is active: {}",
                reasons.join("; ")
            )
        }
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
    let threshold = decision
        .pressures
        .first()
        .map_or(budget.warning_bytes, |pressure| pressure.threshold_bytes);
    warnings.push(ActiveStorageWarning {
        warning_sequence,
        store: store.to_owned(),
        level: decision.level,
        size_bytes: size,
        threshold_bytes: threshold,
        pressures: decision.pressures.clone(),
        message: transition_message(store, decision, size, available),
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
        warning_free_bytes: budget.warning_free_bytes,
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
        warning_free_bytes: budget.warning_free_bytes,
        filesystem_available_bytes: None,
        minimum_free_bytes: budget.minimum_free_bytes,
        level: BudgetLevel::Ok,
    };
    let unavailable_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    StorageMetrics {
        schema_version: STORAGE_METRICS_SCHEMA_VERSION,
        sampled_at: unavailable_at.clone(),
        free_space_checked_at: unavailable_at,
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
    let mut candidate = path;
    loop {
        match fs2::available_space(candidate) {
            Ok(available) => return Ok(available),
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                let Some(parent) = candidate.parent() else {
                    return Err(io_error(candidate, source));
                };
                candidate = parent;
            }
            Err(source) => return Err(io_error(candidate, source)),
        }
    }
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
                warning_free_bytes: 2,
                minimum_free_bytes: 1,
            },
            state_dir: StorageBudgetConfig {
                warning_bytes: warning,
                hard_bytes: hard,
                warning_free_bytes: 2,
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
        let gib = 1024 * 1024 * 1024;
        assert_eq!(free_recovery(100), gib + 100);
        assert_eq!(free_recovery(20 * gib), 22 * gib);
        let budget = StorageBudgetConfig {
            warning_bytes: 1_000,
            hard_bytes: 2_000,
            warning_free_bytes: 4 * gib,
            minimum_free_bytes: 2 * gib,
        };
        let hard = budget_decision(10, 2 * gib - 1, &budget, BudgetLevel::Ok, &[]);
        assert_eq!(hard.level, BudgetLevel::Hard);
        assert_eq!(
            hard.pressures[0].resource,
            StoragePressureResource::FilesystemAvailableBytes
        );

        let still_hard = budget_decision(
            10,
            2 * gib + gib / 2,
            &budget,
            BudgetLevel::Hard,
            &[StoragePressureResource::FilesystemAvailableBytes],
        );
        assert_eq!(still_hard.level, BudgetLevel::Hard);
        let warning = budget_decision(
            10,
            3 * gib,
            &budget,
            BudgetLevel::Hard,
            &[StoragePressureResource::FilesystemAvailableBytes],
        );
        assert_eq!(warning.level, BudgetLevel::Warning);
        assert_eq!(warning.pressures[0].threshold_bytes, 4 * gib);
        let still_warning = budget_decision(
            10,
            4 * gib + gib / 2,
            &budget,
            BudgetLevel::Warning,
            &[StoragePressureResource::FilesystemAvailableBytes],
        );
        assert_eq!(still_warning.level, BudgetLevel::Warning);
        let recovered = budget_decision(
            10,
            5 * gib,
            &budget,
            BudgetLevel::Warning,
            &[StoragePressureResource::FilesystemAvailableBytes],
        );
        assert_eq!(recovered.level, BudgetLevel::Ok);
    }

    #[test]
    fn live_free_space_warning_is_durable_and_escalates_in_one_episode() {
        let root = TempDir::new().unwrap();
        let data = root.path().join("data");
        let state = root.path().join("state");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        let mut policy = config(u64::MAX - 1, u64::MAX);
        policy.data_dir.warning_free_bytes = 100;
        policy.data_dir.minimum_free_bytes = 50;
        policy.state_dir.warning_free_bytes = 100;
        policy.state_dir.minimum_free_bytes = 50;
        let mut monitor = StorageMonitor::open(&data, &state, policy, 0);
        let sampled_at = monitor.snapshot().sampled_at.clone();

        monitor.set_free_space_override(75, 1_000);
        monitor.refresh_free_space().unwrap();
        let warning = monitor.snapshot();
        assert_eq!(warning.sampled_at, sampled_at);
        assert_eq!(warning.data_dir.level, BudgetLevel::Warning);
        assert!(warning.intake.accepting);
        assert_eq!(warning.active_warnings[0].warning_sequence, 1);
        assert_eq!(warning.active_warnings[0].threshold_bytes, 100);
        assert!(warning.active_warnings[0]
            .message
            .contains("filesystem available"));

        monitor.set_free_space_override(49, 1_000);
        monitor.refresh_free_space().unwrap();
        let hard = monitor.snapshot();
        assert_eq!(hard.data_dir.level, BudgetLevel::Hard);
        assert!(!hard.intake.accepting);
        assert_eq!(hard.active_warnings[0].warning_sequence, 1);
        assert_eq!(monitor.take_warnings().len(), 2);
        let durable = std::fs::read_to_string(data.join(STORAGE_WARNING_FILE)).unwrap();
        assert_eq!(durable.lines().count(), 2);
    }

    #[test]
    fn size_thresholds_do_not_flap_inside_recovery_band() {
        let budget = StorageBudgetConfig {
            warning_bytes: 1_000,
            hard_bytes: 2_000,
            warning_free_bytes: 2,
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
            warning_free_bytes: 2,
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
        assert!(filesystem_available(&vanished).unwrap() > 0);
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
