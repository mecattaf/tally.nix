use super::*;

/// The recovery policy this CLI starts its daemon under.
///
/// Named rather than spelled inline so the one place that decides it is
/// findable by name. It has exactly one reader — the daemon entry point below.
/// `query run --durable` deliberately does **not** consume it: the durable view
/// derives row state from the witness ledger rather than by running the
/// recovery planner, so it is not coupled to this policy at all.
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
            // Job limits are optional (vestige-sweep V-1): the flags above
            // honor an explicitly passed value and render nothing when it is
            // absent, so an unauthored cap can no longer ride into every job.
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
