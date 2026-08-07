use super::*;

/// The recovery policy this CLI starts its daemon under.
///
/// Named rather than spelled inline because a second reader now depends on it:
/// `query run --durable` derives row states by running the same recovery
/// derivation over the same durable facts, and a durable view computed under a
/// different policy would report states the daemon would not.
pub(super) const DAEMON_RECOVERY_POLICY: RecoveryPolicy = RecoveryPolicy {
    retry: RetryPolicy {
        auto_pool_return: true,
        auto_resource_return: false,
        auto_bounded_requeue: false,
    },
    max_attempts: 2,
};

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_daemon_runtime(
    config_path: Option<PathBuf>,
    socket: PathBuf,
    cpu_weight: Option<u16>,
    memory_max_bytes: Option<u64>,
    state_dir: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    yield_grace_sec: u64,
) -> Result<()> {
    let config_path = config_path.map_or_else(default_config_path, Ok)?;
    let config = Config::from_path(&config_path)?;
    let cpu_weight = required_daemon_value(cpu_weight, "TALLY_CPU_WEIGHT", "--cpu-weight")?;
    let memory_max_bytes = required_daemon_value(
        memory_max_bytes,
        "TALLY_MEMORY_MAX_BYTES",
        "--memory-max-bytes",
    )?;
    let state_dir = state_dir.map_or_else(default_state_dir, Ok)?;
    let data_dir = data_dir.map_or_else(default_data_dir, Ok)?;
    let recorder_program = std::env::current_exe().context("cannot resolve tally executable")?;
    let daemon = Daemon::open(
        config,
        DaemonPaths {
            socket,
            state_dir,
            data_dir,
        },
        DaemonSettings {
            unit_limits: UnitLimits {
                cpu_weight,
                memory_max_bytes,
            },
            yield_grace: std::time::Duration::from_secs(yield_grace_sec),
            recovery_policy: DAEMON_RECOVERY_POLICY,
            max_connections: DEFAULT_MAX_CONNECTIONS,
        },
        recorder_program,
    )
    .await?;
    daemon.run().await?;
    Ok(())
}

pub(super) fn required_daemon_value<T>(
    cli: Option<T>,
    environment: &'static str,
    flag: &'static str,
) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    if let Some(value) = cli {
        return Ok(value);
    }
    let value = std::env::var(environment)
        .with_context(|| format!("daemon requires {flag} or {environment}"))?;
    value
        .parse()
        .map_err(|error| anyhow::anyhow!("{environment} has an invalid value: {error}"))
}
