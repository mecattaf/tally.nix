use super::*;

pub(super) fn validate_local_unit_fact_shape(
    expected_unit: &str,
    fact: &LocalUnitFact,
) -> Result<(), ExecutorError> {
    let invalid = |detail: String| ExecutorError::UnitProbe {
        unit: expected_unit.to_owned(),
        detail,
    };
    if fact.unit != expected_unit {
        return Err(invalid(format!(
            "probe returned unit {:?}, expected {expected_unit:?}",
            fact.unit
        )));
    }
    match fact.state {
        LocalUnitState::Absent => {
            if fact.loaded
                || fact.invocation_id.is_some()
                || fact.attempt.is_some()
                || fact.lease_epoch.is_some()
                || fact.exit_record.is_some()
            {
                return Err(invalid("absent unit carries execution metadata".to_owned()));
            }
        }
        LocalUnitState::Running => {
            if !fact.loaded
                || fact.invocation_id.as_deref().is_none_or(str::is_empty)
                || fact.attempt.is_none_or(|attempt| attempt == 0)
                || fact.lease_epoch.is_none_or(|epoch| epoch == 0)
                || fact.exit_record.is_some()
            {
                return Err(invalid(
                    "running unit has incomplete or contradictory metadata".to_owned(),
                ));
            }
        }
        LocalUnitState::Exited => {
            let record = fact
                .exit_record
                .as_ref()
                .ok_or_else(|| invalid("exited unit has no durable exit record".to_owned()))?;
            record.validate(expected_unit)?;
            if fact.invocation_id.as_deref() != Some(record.invocation_id.as_str())
                || fact.attempt != Some(record.attempt)
                || fact.lease_epoch != Some(record.lease_epoch)
            {
                return Err(invalid(
                    "exited unit metadata does not match its durable record".to_owned(),
                ));
            }
        }
        LocalUnitState::InactiveWithoutRecord => {
            if !fact.loaded
                || fact.invocation_id.as_deref().is_none_or(str::is_empty)
                || fact.attempt.is_some()
                || fact.lease_epoch.is_some()
                || fact.exit_record.is_some()
            {
                return Err(invalid(
                    "inactive unit has incomplete or contradictory metadata".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

pub trait LocalUnitProbe: Send + Sync {
    fn inspect(&self, unit: &str, paths: &ExecutionPaths) -> Result<LocalUnitFact, ExecutorError>;
}

#[derive(Debug, Clone)]
pub struct SystemdLocalUnitProbe {
    systemctl: PathBuf,
}

impl Default for SystemdLocalUnitProbe {
    fn default() -> Self {
        Self {
            systemctl: PathBuf::from("systemctl"),
        }
    }
}

impl SystemdLocalUnitProbe {
    pub fn with_program(program: impl Into<PathBuf>) -> Self {
        Self {
            systemctl: program.into(),
        }
    }
}

impl LocalUnitProbe for SystemdLocalUnitProbe {
    fn inspect(&self, unit: &str, paths: &ExecutionPaths) -> Result<LocalUnitFact, ExecutorError> {
        let output = std::process::Command::new(&self.systemctl)
            .args([
                "--user",
                "show",
                "--property=LoadState",
                "--property=ActiveState",
                "--property=InvocationID",
                "--property=Environment",
                "--",
                unit,
            ])
            .output()
            .map_err(|source| ExecutorError::UnitProbe {
                unit: unit.to_owned(),
                detail: source.to_string(),
            })?;
        if !output.status.success() {
            return Err(ExecutorError::UnitProbe {
                unit: unit.to_owned(),
                detail: format!(
                    "systemctl --user show failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            });
        }
        interpret_systemd_unit_show(unit, paths, &output.stdout)
    }
}

pub(super) fn interpret_systemd_unit_show(
    unit: &str,
    paths: &ExecutionPaths,
    stdout: &[u8],
) -> Result<LocalUnitFact, ExecutorError> {
    let text = std::str::from_utf8(stdout).map_err(|error| ExecutorError::UnitProbe {
        unit: unit.to_owned(),
        detail: format!("systemctl show output is not UTF-8: {error}"),
    })?;
    let mut properties = HashMap::new();
    for line in text.lines() {
        let (name, value) = line
            .split_once('=')
            .ok_or_else(|| ExecutorError::UnitProbe {
                unit: unit.to_owned(),
                detail: format!("malformed systemctl show line {line:?}"),
            })?;
        if !matches!(
            name,
            "LoadState" | "ActiveState" | "InvocationID" | "Environment"
        ) {
            return Err(ExecutorError::UnitProbe {
                unit: unit.to_owned(),
                detail: format!("unexpected systemctl show property {name:?}"),
            });
        }
        if properties.insert(name, value).is_some() {
            return Err(ExecutorError::UnitProbe {
                unit: unit.to_owned(),
                detail: format!("duplicate systemctl show property {name:?}"),
            });
        }
    }
    let required = |name: &'static str| {
        properties
            .get(name)
            .copied()
            .ok_or_else(|| ExecutorError::UnitProbe {
                unit: unit.to_owned(),
                detail: format!("systemctl show omitted {name}"),
            })
    };
    let load_state = required("LoadState")?;
    let active_state = required("ActiveState")?;
    let invocation_id = required("InvocationID")?;
    let environment = required("Environment")?;
    let exit_record = match read_exit_record(&paths.exit_record, unit) {
        Ok(record) => Some(record),
        Err(error) if is_not_found(&error) => None,
        Err(error) => return Err(error),
    };

    if load_state == "not-found" {
        if active_state != "inactive" || !invocation_id.is_empty() || !environment.is_empty() {
            return Err(ExecutorError::UnitProbe {
                unit: unit.to_owned(),
                detail: format!(
                    "not-found unit reported ActiveState={active_state:?}, InvocationID={invocation_id:?}, or a non-empty Environment"
                ),
            });
        }
        return match exit_record {
            Some(record) => Ok(LocalUnitFact {
                unit: unit.to_owned(),
                loaded: false,
                state: LocalUnitState::Exited,
                invocation_id: Some(record.invocation_id.clone()),
                attempt: Some(record.attempt),
                lease_epoch: Some(record.lease_epoch),
                exit_record: Some(record),
            }),
            None => Ok(LocalUnitFact::absent(unit)),
        };
    }
    if load_state != "loaded" {
        return Err(ExecutorError::UnitProbe {
            unit: unit.to_owned(),
            detail: format!("unsupported LoadState {load_state:?}"),
        });
    }
    if invocation_id.is_empty() {
        return Err(ExecutorError::UnitProbe {
            unit: unit.to_owned(),
            detail: "loaded unit has no InvocationID".to_owned(),
        });
    }
    if let Some(record) = exit_record {
        if record.invocation_id != invocation_id {
            return Err(ExecutorError::UnitProbe {
                unit: unit.to_owned(),
                detail: format!(
                    "durable exit InvocationID {:?} does not match live unit InvocationID {invocation_id:?}",
                    record.invocation_id
                ),
            });
        }
        return Ok(LocalUnitFact {
            unit: unit.to_owned(),
            loaded: true,
            state: LocalUnitState::Exited,
            invocation_id: Some(invocation_id.to_owned()),
            attempt: Some(record.attempt),
            lease_epoch: Some(record.lease_epoch),
            exit_record: Some(record),
        });
    }
    match active_state {
        "active" | "activating" | "reloading" | "deactivating" => {
            let (attempt, lease_epoch) = execution_metadata_from_environment(unit, environment)?;
            Ok(LocalUnitFact {
                unit: unit.to_owned(),
                loaded: true,
                state: LocalUnitState::Running,
                invocation_id: Some(invocation_id.to_owned()),
                attempt: Some(attempt),
                lease_epoch: Some(lease_epoch),
                exit_record: None,
            })
        }
        "inactive" | "failed" => Ok(LocalUnitFact {
            unit: unit.to_owned(),
            loaded: true,
            state: LocalUnitState::InactiveWithoutRecord,
            invocation_id: Some(invocation_id.to_owned()),
            attempt: None,
            lease_epoch: None,
            exit_record: None,
        }),
        other => Err(ExecutorError::UnitProbe {
            unit: unit.to_owned(),
            detail: format!("unsupported ActiveState {other:?}"),
        }),
    }
}

pub(super) fn execution_metadata_from_environment(
    unit: &str,
    environment: &str,
) -> Result<(u32, u64), ExecutorError> {
    let words = split_systemd_words(environment).map_err(|detail| ExecutorError::UnitProbe {
        unit: unit.to_owned(),
        detail,
    })?;
    let mut attempt = None;
    let mut lease_epoch = None;
    for word in words {
        let Some((name, value)) = word.split_once('=') else {
            return Err(ExecutorError::UnitProbe {
                unit: unit.to_owned(),
                detail: format!("malformed unit environment word {word:?}"),
            });
        };
        match name {
            "TALLY_ATTEMPT" => {
                let parsed = value
                    .parse::<u32>()
                    .ok()
                    .filter(|value| *value > 0)
                    .ok_or_else(|| ExecutorError::UnitProbe {
                        unit: unit.to_owned(),
                        detail: format!("invalid TALLY_ATTEMPT {value:?}"),
                    })?;
                if attempt.replace(parsed).is_some() {
                    return Err(ExecutorError::UnitProbe {
                        unit: unit.to_owned(),
                        detail: "duplicate TALLY_ATTEMPT".to_owned(),
                    });
                }
            }
            "TALLY_LEASE_EPOCH" => {
                let parsed = value
                    .parse::<u64>()
                    .ok()
                    .filter(|value| *value > 0)
                    .ok_or_else(|| ExecutorError::UnitProbe {
                        unit: unit.to_owned(),
                        detail: format!("invalid TALLY_LEASE_EPOCH {value:?}"),
                    })?;
                if lease_epoch.replace(parsed).is_some() {
                    return Err(ExecutorError::UnitProbe {
                        unit: unit.to_owned(),
                        detail: "duplicate TALLY_LEASE_EPOCH".to_owned(),
                    });
                }
            }
            _ => {}
        }
    }
    match (attempt, lease_epoch) {
        (Some(attempt), Some(lease_epoch)) => Ok((attempt, lease_epoch)),
        _ => Err(ExecutorError::UnitProbe {
            unit: unit.to_owned(),
            detail: "unit Environment omitted TALLY_ATTEMPT or TALLY_LEASE_EPOCH".to_owned(),
        }),
    }
}

pub(super) fn split_systemd_words(input: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;
    for character in input.chars() {
        if escaped {
            word.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            } else {
                word.push(character);
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            character if character.is_whitespace() => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            other => word.push(other),
        }
    }
    if escaped || quote.is_some() {
        return Err("unterminated quoting in unit Environment".to_owned());
    }
    if !word.is_empty() {
        words.push(word);
    }
    Ok(words)
}

/// The properties the exit recorder's one accounting `systemctl show` reads.
/// `CPUUsageNSec` is the generic per-job charge; the two monotonic
/// timestamps give a GPU-pool job's main-process wall-clock runtime,
/// measured by systemd's own clock rather than the daemon's dispatch-side
/// `Instant`. That is a lower bound on how long the job actually held its
/// pool lease, not the lease span itself — the lease is held from admission
/// through completion handling, which strictly contains `ExecMain`'s
/// lifetime.
///
/// The three memory properties make an OOM kill legible (vestige-sweep V-3):
/// `MemoryMax` is the effective cap the failure fact must name, `MemoryPeak`
/// the observed high-water mark, and `MemoryEvents` carries the cgroup
/// `memory.events` counters — including `oom_kill`, the only artifact
/// anywhere that records the child-kill shape (the kernel kills a child like
/// `rustc`, the agent survives, and the unit exits with the agent's own
/// status). All of them ride the single invocation this probe has always
/// issued; a systemd too old to expose one of them leaves that field a typed
/// absence.
const ACCOUNTING_PROPERTIES: [&str; 6] = [
    "CPUUsageNSec",
    "ExecMainStartTimestampMonotonic",
    "ExecMainExitTimestampMonotonic",
    "MemoryMax",
    "MemoryPeak",
    "MemoryEvents",
];

/// Issues the exit recorder's one accounting probe. Called from
/// `ExecStopPost`, while the transient unit is still `deactivating` /
/// `stop-post` and therefore still queryable — after the unit is collected
/// (which can happen within seconds) every property below reads back empty.
///
/// A spawn failure, non-zero exit, or malformed line fails the whole probe;
/// the caller logs and stores a typed absence rather than treating any of
/// this as fatal to the exit record itself. Accounting is advisory.
pub(super) fn probe_unit_accounting(
    systemctl: &Path,
    unit: &str,
) -> Result<UnitAccounting, ExecutorError> {
    let mut command = std::process::Command::new(systemctl);
    command.arg("--user").arg("show");
    for property in ACCOUNTING_PROPERTIES {
        command.arg(format!("--property={property}"));
    }
    command.arg("--").arg(unit);
    let output = command
        .output()
        .map_err(|source| ExecutorError::UnitProbe {
            unit: unit.to_owned(),
            detail: source.to_string(),
        })?;
    if !output.status.success() {
        return Err(ExecutorError::UnitProbe {
            unit: unit.to_owned(),
            detail: format!(
                "systemctl --user show (accounting) failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    parse_unit_accounting(unit, &output.stdout)
}

pub(super) fn parse_unit_accounting(
    unit: &str,
    stdout: &[u8],
) -> Result<UnitAccounting, ExecutorError> {
    let text = std::str::from_utf8(stdout).map_err(|error| ExecutorError::UnitProbe {
        unit: unit.to_owned(),
        detail: format!("systemctl show (accounting) output is not UTF-8: {error}"),
    })?;
    let mut properties = HashMap::new();
    for line in text.lines() {
        let (name, value) = line
            .split_once('=')
            .ok_or_else(|| ExecutorError::UnitProbe {
                unit: unit.to_owned(),
                detail: format!("malformed systemctl show (accounting) line {line:?}"),
            })?;
        if !ACCOUNTING_PROPERTIES.contains(&name) {
            return Err(ExecutorError::UnitProbe {
                unit: unit.to_owned(),
                detail: format!("unexpected systemctl show (accounting) property {name:?}"),
            });
        }
        if properties.insert(name, value).is_some() {
            return Err(ExecutorError::UnitProbe {
                unit: unit.to_owned(),
                detail: format!("duplicate systemctl show (accounting) property {name:?}"),
            });
        }
    }
    // `ExecMain{Start,Exit}TimestampMonotonic` report an unknown/never-run
    // unit's timestamp as the literal `0`, not `[not set]` — systemd's own
    // "this never happened" for a monotonic clock that is never exactly
    // zero at any real point after boot. `CPUUsageNSec` has no such
    // sentinel: `0` there is a plausible real measurement, so only the two
    // timestamp properties treat it as absence.
    let parse_property = |name: &'static str,
                          zero_is_unset: bool|
     -> Result<Option<u64>, ExecutorError> {
        match properties.get(name).copied() {
            None | Some("[not set]") | Some("") => Ok(None),
            Some("0") if zero_is_unset => Ok(None),
            Some(value) => value
                .parse::<u64>()
                .map(Some)
                .map_err(|_| ExecutorError::UnitProbe {
                    unit: unit.to_owned(),
                    detail: format!(
                        "systemctl show (accounting) property {name} is not a u64: {value:?}"
                    ),
                }),
        }
    };
    // A byte-count property that reports "no limit" as systemd's literal
    // `infinity` rather than a number (`MemoryMax` on an uncapped unit), and
    // "never measured" as `[not set]`. Both are typed absences: an unauthored
    // cap must never materialize as an invented number on the record, and an
    // absent cap is itself the fact the failure fact names (vestige-sweep
    // V-1, V-3).
    let parse_bytes = |name: &'static str| -> Result<Option<u64>, ExecutorError> {
        match properties.get(name).copied() {
            None | Some("[not set]") | Some("") | Some("infinity") => Ok(None),
            Some(value) => value
                .parse::<u64>()
                .map(Some)
                .map_err(|_| ExecutorError::UnitProbe {
                    unit: unit.to_owned(),
                    detail: format!(
                        "systemctl show (accounting) property {name} is not a byte count: {value:?}"
                    ),
                }),
        }
    };
    // `MemoryEvents` is systemd's JSON rendering of the cgroup `memory.events`
    // counters. The one counter this probe exists to lift out is `oom_kill`:
    // a nonzero count is the only durable evidence of the child-kill shape
    // (vestige-sweep V-3). A systemd that does not expose the property leaves
    // the counter unmeasured; a malformed rendering fails the whole probe, the
    // same way every other malformed line does, and the caller stores a typed
    // absence.
    let oom_kill_count = match properties.get("MemoryEvents").copied() {
        None | Some("[not set]") | Some("") => None,
        Some(events) => {
            let parsed: Value =
                serde_json::from_str(events).map_err(|_| ExecutorError::UnitProbe {
                    unit: unit.to_owned(),
                    detail: format!(
                        "systemctl show (accounting) MemoryEvents is not JSON: {events:?}"
                    ),
                })?;
            match parsed.get("oom_kill") {
                Some(value) => Some(value.as_u64().ok_or_else(|| {
                    ExecutorError::UnitProbe {
                        unit: unit.to_owned(),
                        detail: format!(
                            "systemctl show (accounting) MemoryEvents oom_kill is not a u64: {events:?}"
                        ),
                    }
                })?),
                None => None,
            }
        }
    };
    Ok(UnitAccounting {
        cpu_usage_nsec: parse_property("CPUUsageNSec", false)?,
        exec_main_start_monotonic_usec: parse_property("ExecMainStartTimestampMonotonic", true)?,
        exec_main_exit_monotonic_usec: parse_property("ExecMainExitTimestampMonotonic", true)?,
        memory_max_bytes: parse_bytes("MemoryMax")?,
        memory_peak_bytes: parse_bytes("MemoryPeak")?,
        oom_kill_count,
    })
}
