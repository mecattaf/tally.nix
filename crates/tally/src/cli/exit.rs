use super::*;

#[derive(Debug)]
pub(super) struct ExitFailure {
    pub(super) code: i32,
    pub(super) message: String,
}

impl std::fmt::Display for ExitFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl StdError for ExitFailure {}

pub(super) fn invalid(message: impl Into<String>) -> anyhow::Error {
    exit_failure(2, message)
}

pub(super) fn exit_failure(code: i32, message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(ExitFailure {
        code,
        message: message.into(),
    })
}

/// Whether this failure is "there is no daemon listening on that socket".
///
/// Deliberately narrower than "exit code 3": `RearmDeadlineExceeded` maps to
/// the same code but describes a daemon that is present and not answering,
/// which is a real fault. Only the connect-time absence raised at
/// [`WireIoError::Unreachable`] is an absence.
pub(super) fn is_daemon_absent(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| matches!(cause.downcast_ref(), Some(WireIoError::Unreachable { .. })))
}

pub(super) fn error_exit_code(error: &anyhow::Error) -> i32 {
    for cause in error.chain() {
        if let Some(failure) = cause.downcast_ref::<ExitFailure>() {
            return failure.code;
        }
        if let Some(wire) = cause.downcast_ref::<WireIoError>() {
            return match wire {
                WireIoError::Unreachable { .. } | WireIoError::RearmDeadlineExceeded { .. } => 3,
                WireIoError::Rpc(WireErrorCode::InvalidParams, _, _) => 2,
                WireIoError::Rpc(WireErrorCode::NotFound, _, _) => 4,
                _ => 1,
            };
        }
    }
    1
}

pub(super) fn verdict_exit_code(verdict: &str) -> i32 {
    match verdict {
        "pass" | "reused" | "substituted" => 0,
        "clean-exit-no-artifact" => 3,
        "cancelled" => 4,
        "failed" | "pool-vanished" | "preempted" | "runtime-exceeded" => 1,
        _ => 1,
    }
}

pub(super) fn waited_exit_code(waited: &Value) -> i32 {
    waited
        .get("verdict")
        .and_then(Value::as_str)
        .map(verdict_exit_code)
        .or_else(|| {
            waited
                .get("exit_code")
                .and_then(Value::as_i64)
                .map(|value| {
                    if value == 0 {
                        0
                    } else {
                        value.clamp(1, 255) as i32
                    }
                })
        })
        .unwrap_or(1)
}

pub(super) fn inherited_caller_job_id() -> Option<String> {
    std::env::var("TALLY_JOB_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

pub(super) fn inherited_caller_job_token() -> Option<String> {
    std::env::var("TALLY_JOB_TOKEN")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

pub(super) fn resolve_rpc_timeout(
    flag: Option<u64>,
    environment: Option<&OsStr>,
) -> Result<Duration> {
    let seconds = match flag {
        Some(seconds) => seconds,
        None => match environment {
            Some(value) => value
                .to_str()
                .ok_or_else(|| invalid(format!("{RPC_TIMEOUT_ENV} must be valid UTF-8")))?
                .parse::<u64>()
                .map_err(|_| {
                    invalid(format!(
                        "{RPC_TIMEOUT_ENV} must be a positive whole number of seconds"
                    ))
                })?,
            None => DEFAULT_RPC_TIMEOUT_SEC,
        },
    };
    if seconds == 0 {
        return Err(invalid(
            "--rpc-timeout-sec and TALLY_RPC_TIMEOUT_SEC must be greater than zero",
        ));
    }
    Ok(Duration::from_secs(seconds))
}

pub(super) fn default_socket_path() -> PathBuf {
    if let Some(socket) = std::env::var_os("TALLY_SOCKET") {
        return PathBuf::from(socket);
    }
    std::env::var_os("XDG_RUNTIME_DIR").map_or_else(
        || std::env::temp_dir().join("tally/tally.sock"),
        |runtime| PathBuf::from(runtime).join("tally/tally.sock"),
    )
}

pub(super) fn default_state_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_STATE_HOME") {
        return Ok(PathBuf::from(path).join("tally"));
    }
    let home = std::env::var_os("HOME").context("HOME and XDG_STATE_HOME are both unset")?;
    Ok(PathBuf::from(home).join(".local/state/tally"))
}

pub(super) fn default_data_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_DATA_HOME") {
        return Ok(PathBuf::from(path).join("tally"));
    }
    let home = std::env::var_os("HOME").context("HOME and XDG_DATA_HOME are both unset")?;
    Ok(PathBuf::from(home).join(".local/share/tally"))
}
