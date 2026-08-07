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

/// Whether this failure is the drain RPC's client deadline expiring on an
/// ESTABLISHED connection (#427).
///
/// The daemon is present — the socket connected — but too busy to answer
/// `queue.drain` within the client's deadline. The producer event files are
/// durable on disk and the next `tally-drain` tick picks them up, so nothing
/// is lost: for the periodic drain this is a retryable skip, symmetric with
/// #411's connect-time absence handling. Deliberately narrow: only
/// `queue.drain`'s own client deadline counts. A drained rearm window
/// (`RearmDeadlineExceeded`) is a different path this verb never takes, and
/// every other established-connection failure — including a daemon that is
/// listening and refuses — keeps failing the unit.
pub(super) fn is_drain_deadline_exceeded(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        matches!(
            cause.downcast_ref(),
            Some(WireIoError::DeadlineExceeded { method, .. }) if method == "queue.drain"
        )
    })
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

/// The default data directory for the direct-file verbs (#416).
///
/// Precedence: an explicit `--data-dir` flag wins at every call site (each
/// resolves the flag first and only falls back here), then `TALLY_DATA_DIR`
/// taken verbatim as the directory itself, then the XDG default
/// (`$XDG_DATA_HOME/tally`, else `~/.local/share/tally`). The variable is
/// what a deployment exports so the whole verb family — reader-state,
/// `witness verify`, and the rest — aims at the deployment's store by
/// default; unset or empty, local use keeps resolving exactly as before.
pub(super) fn default_data_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("TALLY_DATA_DIR").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("XDG_DATA_HOME") {
        return Ok(PathBuf::from(path).join("tally"));
    }
    let home = std::env::var_os("HOME")
        .context("TALLY_DATA_DIR is unset or empty and HOME and XDG_DATA_HOME are both unset")?;
    Ok(PathBuf::from(home).join(".local/share/tally"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #411: the absorption is scoped to "there is no daemon there", and
    /// that scope is the whole of what keeps it honest.
    ///
    /// `RearmDeadlineExceeded` maps to the same exit code 3 and is the reason
    /// this cannot be written as "is the exit code 3" — it describes a daemon
    /// that is present and not answering, which is a real fault and must keep
    /// failing the drain. Nothing else in the tree pins that distinction: a
    /// widened predicate leaves every `tally` test binary green, because the
    /// only fixture that reaches this path is an absent socket.
    #[test]
    fn only_a_connect_time_absence_counts_as_an_absent_daemon() {
        let absent = anyhow::Error::new(WireIoError::Unreachable {
            path: PathBuf::from("/run/user/0/tally/tally.sock"),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        });
        assert!(is_daemon_absent(&absent));
        assert_eq!(error_exit_code(&absent), 3);

        // Present, and not answering. Same exit code, not an absence.
        let wedged = anyhow::Error::new(WireIoError::RearmDeadlineExceeded {
            method: "queue.drain".to_owned(),
            path: PathBuf::from("/run/user/0/tally/tally.sock"),
            window: Duration::from_secs(5),
        });
        assert_eq!(
            error_exit_code(&wedged),
            3,
            "the point of this test is that the exit code cannot tell them apart"
        );
        assert!(
            !is_daemon_absent(&wedged),
            "a daemon that is listening and not answering is a fault, not an absence"
        );

        // Answering, and refusing.
        let refused = anyhow::Error::new(WireIoError::Rpc(
            WireErrorCode::InvalidParams,
            "drain refused".to_owned(),
            None,
        ));
        assert!(!is_daemon_absent(&refused));

        // Wrapped in context the way the call sites raise it.
        let wrapped = anyhow::Error::new(WireIoError::Unreachable {
            path: PathBuf::from("/run/user/0/tally/tally.sock"),
            source: std::io::Error::from(std::io::ErrorKind::ConnectionRefused),
        })
        .context("draining the queue");
        assert!(is_daemon_absent(&wrapped));

        // #427: the deadline skip is a separate predicate, and the absence
        // predicate must not grow to cover it — a busy daemon answering
        // nothing is still present.
        let busy = anyhow::Error::new(WireIoError::DeadlineExceeded {
            method: "queue.drain".to_owned(),
            deadline: Duration::from_secs(60),
        });
        assert!(!is_daemon_absent(&busy));
        assert_eq!(
            error_exit_code(&busy),
            1,
            "a deadline expiry keeps the ordinary failure exit until the drain absorbs it"
        );
    }

    /// Issue #427: the absorption is scoped to "the drain's own RPC deadline
    /// expired on a connection that was established", and that scope is what
    /// keeps it honest. Nothing is lost — the event files are durable and the
    /// next tick drains them — but the same latitude for any other error would
    /// turn a genuinely failing drain into a quiet success.
    #[test]
    fn only_the_drain_deadline_counts_as_a_retryable_drain_skip() {
        // The case itself: connected, answered nothing within the deadline.
        let busy = anyhow::Error::new(WireIoError::DeadlineExceeded {
            method: "queue.drain".to_owned(),
            deadline: Duration::from_secs(60),
        });
        assert!(is_drain_deadline_exceeded(&busy));

        // Wrapped in context the way the call sites raise it.
        let wrapped = anyhow::Error::new(WireIoError::DeadlineExceeded {
            method: "queue.drain".to_owned(),
            deadline: Duration::from_secs(1),
        })
        .context("draining the queue");
        assert!(is_drain_deadline_exceeded(&wrapped));

        // Another method's deadline is not the drain's skip: the predicate
        // names its method, not the error variant alone.
        let other_method = anyhow::Error::new(WireIoError::DeadlineExceeded {
            method: "queue.await_job".to_owned(),
            deadline: Duration::from_secs(60),
        });
        assert!(
            !is_drain_deadline_exceeded(&other_method),
            "only queue.drain's own deadline is a retryable skip"
        );

        // A drained rearm window is present-and-not-answering on the
        // reconnect path, not a single-call deadline: not the skip.
        let rearm = anyhow::Error::new(WireIoError::RearmDeadlineExceeded {
            method: "queue.drain".to_owned(),
            path: PathBuf::from("/run/user/0/tally/tally.sock"),
            window: Duration::from_secs(60),
        });
        assert!(
            !is_drain_deadline_exceeded(&rearm),
            "a rearm-window exhaustion is a different fault than a call deadline"
        );

        // A daemon that is listening and refuses is still a failure.
        let refused = anyhow::Error::new(WireIoError::Rpc(
            WireErrorCode::InvalidParams,
            "drain refused".to_owned(),
            None,
        ));
        assert!(!is_drain_deadline_exceeded(&refused));

        // The socket-absent case is #411's predicate, not this one.
        let absent = anyhow::Error::new(WireIoError::Unreachable {
            path: PathBuf::from("/run/user/0/tally/tally.sock"),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        });
        assert!(!is_drain_deadline_exceeded(&absent));
    }
}
