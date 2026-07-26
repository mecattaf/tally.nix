use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::process::Command;

use crate::completion::SemanticCompletion;
use crate::config::{GitAiConfig, GitAiMode};
use crate::witness::{current_host_id, Authorship, AuthorshipSession, AuthorshipStatus};

const PROGRAM: &str = "git-ai";
const NOTE_REF: &str = "refs/notes/ai";
const NOTE_DIVIDER: &[u8] = b"\n---\n";
const FAMILY_ADAPTER_VERSION: &str = "1.6.17";
const CONTROL_RESPONSE_LIMIT: usize = 64 * 1024;
const DAEMON_START_TIMEOUT: Duration = Duration::from_secs(10);
const PRIVATE_DAEMON_DIRECTORY: &str = "git-ai";
const PRIVATE_DAEMON_PREFIX: &str = "tally-git-ai";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GitAiExecution {
    pub config: GitAiConfig,
    pub attributes: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_model: Option<String>,
}

impl GitAiExecution {
    pub fn validate(&self) -> Result<(), String> {
        if !self.config.enable {
            return Err("git-ai execution settings require gitAi.enable=true".to_owned());
        }
        if self.config.await_timeout_sec == 0 {
            return Err("git-ai await timeout must be positive".to_owned());
        }
        let required = ["taskUuid", "attempt", "leaseEpoch", "adapter"];
        if self.attributes.len() > 6
            || required
                .iter()
                .any(|name| self.attributes.get(*name).is_none_or(String::is_empty))
            || self.attributes.keys().any(|name| {
                !required.contains(&name.as_str()) && name != "flowRunId" && name != "nodeOrdinal"
            })
            || self.attributes.values().any(|value| {
                value.is_empty() || value.len() > 256 || value.chars().any(char::is_control)
            })
        {
            return Err(
                "git-ai custom attributes are not the bounded Tally correlation set".to_owned(),
            );
        }
        for (name, value) in [
            ("expected session", self.expected_session.as_deref()),
            ("expected model", self.expected_model.as_deref()),
        ] {
            if value.is_some_and(|value| {
                value.is_empty() || value.len() > 1024 || value.chars().any(char::is_control)
            }) {
                return Err(format!("git-ai {name} is not a bounded scalar"));
            }
        }
        Ok(())
    }

    pub fn attributes_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(&self.attributes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Preflight {
    Available {
        provider_version: String,
        provider_program: PathBuf,
    },
    Failed {
        provider_version: String,
        status: AuthorshipStatus,
        reason: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Binding {
    pub result_revision: Option<String>,
    pub authorship: Option<Authorship>,
    pub authorship_sessions: Option<Vec<AuthorshipSession>>,
    pub required_failure: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct CommandContext {
    path: Option<OsString>,
    environment: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaemonBackend {
    Systemd,
    Direct,
}

#[derive(Debug)]
pub(crate) struct PrivateDaemon {
    provider_program: PathBuf,
    provider_version: String,
    daemon_home: PathBuf,
    private_home: PathBuf,
    control_socket: PathBuf,
    trace_socket: PathBuf,
    unit: String,
    systemctl: PathBuf,
    backend: DaemonBackend,
    armed: bool,
}

pub(crate) struct PrivateDaemonLaunch<'a> {
    pub state_dir: &'a Path,
    pub runtime_key: &'a str,
    pub worktree: &'a Path,
    pub repository_write_paths: &'a [PathBuf],
    pub systemd_run: &'a Path,
    pub systemctl: &'a Path,
    pub allow_direct_fallback: bool,
}

impl PrivateDaemon {
    pub(crate) fn child_environment(&self) -> Vec<(String, String)> {
        vec![
            (
                "GIT_AI_DAEMON_HOME".to_owned(),
                self.daemon_home.to_string_lossy().into_owned(),
            ),
            (
                "GIT_AI_DAEMON_CONTROL_SOCKET".to_owned(),
                self.control_socket.to_string_lossy().into_owned(),
            ),
            (
                "GIT_AI_DAEMON_TRACE_SOCKET".to_owned(),
                self.trace_socket.to_string_lossy().into_owned(),
            ),
            (
                "GIT_TRACE2_EVENT".to_owned(),
                format!("af_unix:stream:{}", self.trace_socket.to_string_lossy()),
            ),
            (
                "_GITAI_INTERNAL_DISABLE_WRAPPER_DAEMON_AUTOSPAWN".to_owned(),
                "1".to_owned(),
            ),
        ]
    }

    fn command_context(&self) -> CommandContext {
        CommandContext {
            path: None,
            environment: BTreeMap::from([
                (
                    "GIT_AI_DAEMON_HOME".to_owned(),
                    self.daemon_home.to_string_lossy().into_owned(),
                ),
                (
                    "GIT_AI_DAEMON_CONTROL_SOCKET".to_owned(),
                    self.control_socket.to_string_lossy().into_owned(),
                ),
                (
                    "GIT_AI_DAEMON_TRACE_SOCKET".to_owned(),
                    self.trace_socket.to_string_lossy().into_owned(),
                ),
                (
                    "HOME".to_owned(),
                    self.private_home.to_string_lossy().into_owned(),
                ),
            ]),
        }
    }

    async fn settle_family(&self, worktree: &Path, timeout: Duration) -> Result<(), String> {
        if self.provider_version == FAMILY_ADAPTER_VERSION {
            return send_control_request(
                &self.control_socket,
                &ControlRequest::SyncFamily {
                    repo_working_dir: utf8_path(worktree, "execution worktree")?.to_owned(),
                },
                timeout,
            )
            .await;
        }
        Err(format!(
            "git-ai-error: git-ai {} is unsupported by Tally's version-pinned sync.family adapter (expected {FAMILY_ADAPTER_VERSION})",
            self.provider_version
        ))
    }

    async fn settle_global_fallback(
        &self,
        worktree: &Path,
        timeout_sec: u64,
    ) -> Result<(), String> {
        let timeout = timeout_sec.to_string();
        let result = tokio::time::timeout(
            Duration::from_secs(timeout_sec.saturating_add(1)),
            command_output(
                &self.provider_program,
                ["await", "--timeout", timeout.as_str()],
                Some(worktree),
                &self.command_context(),
            ),
        )
        .await;
        match result {
            Err(_) => Err(format!(
                "git-ai-error: isolated git-ai await timed out after {timeout_sec} seconds"
            )),
            Ok(Ok(output)) if output.status.success() => Ok(()),
            Ok(Ok(output)) => Err(format!(
                "git-ai-unavailable: isolated git-ai await failed: {}",
                command_detail(&output)
            )),
            Ok(Err(error)) => Err(format!(
                "git-ai-unavailable: cannot execute isolated git-ai await: {error}"
            )),
        }
    }

    pub(crate) async fn shutdown(mut self) {
        let _ = send_control_request(
            &self.control_socket,
            &ControlRequest::Shutdown,
            Duration::from_secs(2),
        )
        .await;
        match self.backend {
            DaemonBackend::Systemd => {
                let _ = tokio::time::timeout(
                    Duration::from_secs(5),
                    Command::new(&self.systemctl)
                        .args(["--user", "stop", self.unit.as_str()])
                        .output(),
                )
                .await;
            }
            DaemonBackend::Direct => {
                let _ = tokio::time::timeout(
                    Duration::from_secs(5),
                    command_output(
                        &self.provider_program,
                        ["bg", "shutdown", "--hard"],
                        None,
                        &self.command_context(),
                    ),
                )
                .await;
            }
        }
        self.armed = false;
    }
}

impl Drop for PrivateDaemon {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        best_effort_sync_shutdown(&self.control_socket);
        if self.backend == DaemonBackend::Systemd {
            let _ = std::process::Command::new(&self.systemctl)
                .args(["--user", "stop", self.unit.as_str()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "method", content = "params")]
enum ControlRequest {
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "sync.family")]
    SyncFamily { repo_working_dir: String },
    #[serde(rename = "shutdown")]
    Shutdown,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlResponse {
    ok: bool,
    #[serde(default, rename = "seq")]
    _seq: Option<u64>,
    #[serde(default, rename = "data")]
    _data: Option<Value>,
    #[serde(default)]
    error: Option<String>,
}

pub(crate) async fn preflight(execution: &GitAiExecution) -> Result<Preflight, String> {
    preflight_with_context(execution, &CommandContext::default()).await
}

async fn preflight_with_context(
    execution: &GitAiExecution,
    context: &CommandContext,
) -> Result<Preflight, String> {
    let provider_program = resolve_program(PROGRAM, context).ok_or_else(unavailable_reason);
    let output = match provider_program.as_ref() {
        Ok(program) => command_output(program, ["--version"], None, context).await,
        Err(_) => Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
    };
    let failure = match output {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if !version.is_empty() && version.len() <= 256 && !version.chars().any(char::is_control)
            {
                return Ok(Preflight::Available {
                    provider_version: version,
                    provider_program: provider_program.expect("successful lookup"),
                });
            }
            (
                AuthorshipStatus::Error,
                "git-ai-error: git-ai --version returned an invalid version".to_owned(),
            )
        }
        Ok(output) => (
            AuthorshipStatus::Unavailable,
            format!(
                "git-ai-unavailable: git-ai --version failed on {}: {}",
                hostname(),
                command_detail(&output)
            ),
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            (AuthorshipStatus::Unavailable, unavailable_reason())
        }
        Err(error) => (
            AuthorshipStatus::Unavailable,
            format!(
                "git-ai-unavailable: cannot execute git-ai --version on {}: {error}",
                hostname()
            ),
        ),
    };
    if execution.config.mode == GitAiMode::Required {
        Err(failure.1)
    } else {
        Ok(Preflight::Failed {
            provider_version: "unavailable".to_owned(),
            status: failure.0,
            reason: failure.1,
        })
    }
}

pub(crate) async fn start_private_daemon(
    execution: &GitAiExecution,
    preflight: &Preflight,
    launch: PrivateDaemonLaunch<'_>,
) -> Result<PrivateDaemon, String> {
    let Preflight::Available {
        provider_version,
        provider_program,
    } = preflight
    else {
        return Err("git-ai-error: private daemon requires a successful preflight".to_owned());
    };
    let mut runtime = private_daemon_paths(
        provider_program,
        provider_version,
        launch.state_dir,
        launch.runtime_key,
        launch.systemctl,
    )?;
    prepare_private_directory(&runtime.daemon_home)?;
    prepare_private_directory(&runtime.private_home)?;

    if ping_until(&runtime.control_socket, Duration::from_millis(250))
        .await
        .is_ok()
    {
        return Ok(runtime);
    }

    let attributes = execution
        .attributes_json()
        .map_err(|error| format!("git-ai-error: cannot encode custom attributes: {error}"))?;
    let path = std::env::var_os("PATH")
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut writable = vec![runtime.daemon_home.clone(), runtime.private_home.clone()];
    writable.push(launch.worktree.to_owned());
    writable.extend(launch.repository_write_paths.iter().cloned());
    writable.sort();
    writable.dedup();
    let writable = writable
        .iter()
        .map(|path| quote_systemd_word(path.as_os_str()))
        .collect::<Result<Vec<_>, _>>()?
        .join(" ");

    let mut args = vec![
        "--user".into(),
        "--collect".into(),
        "--quiet".into(),
        "--expand-environment=no".into(),
        "--unit".into(),
        runtime.unit.clone().into(),
    ];
    push_systemd_property(&mut args, "Type=exec");
    push_systemd_property(&mut args, "UMask=0077");
    push_systemd_property(&mut args, "ProtectHome=read-only");
    push_systemd_property(&mut args, "PrivateTmp=yes");
    push_systemd_property(&mut args, "ProtectSystem=strict");
    push_systemd_property(&mut args, "NoNewPrivileges=yes");
    push_systemd_property(&mut args, "RestrictAddressFamilies=AF_UNIX");
    push_systemd_property(&mut args, format!("ReadWritePaths={writable}"));
    for (name, value) in [
        (
            "GIT_AI_DAEMON_HOME",
            utf8_path(&runtime.daemon_home, "private daemon home")?,
        ),
        (
            "GIT_AI_DAEMON_CONTROL_SOCKET",
            utf8_path(&runtime.control_socket, "private daemon control socket")?,
        ),
        (
            "GIT_AI_DAEMON_TRACE_SOCKET",
            utf8_path(&runtime.trace_socket, "private daemon trace socket")?,
        ),
        ("GIT_AI_CUSTOM_ATTRIBUTES", attributes.as_str()),
        ("HOME", utf8_path(&runtime.private_home, "private HOME")?),
        ("PATH", path.as_str()),
    ] {
        args.push("--setenv".into());
        args.push(format!("{name}={value}").into());
    }
    args.push("--working-directory".into());
    args.push(launch.worktree.as_os_str().to_owned());
    args.push("--".into());
    args.push(provider_program.as_os_str().to_owned());
    args.extend(["bg".into(), "run".into()]);

    let launch_result = tokio::time::timeout(
        DAEMON_START_TIMEOUT,
        Command::new(launch.systemd_run).args(&args).output(),
    )
    .await;
    let backend = match launch_result {
        Ok(Ok(output)) if output.status.success() => DaemonBackend::Systemd,
        Ok(Err(error))
            if error.kind() == std::io::ErrorKind::NotFound && launch.allow_direct_fallback =>
        {
            start_direct_daemon(&runtime, execution, &path).await?;
            DaemonBackend::Direct
        }
        Err(_) => {
            return Err(format!(
                "git-ai-error: private git-ai daemon launcher timed out after {} seconds",
                DAEMON_START_TIMEOUT.as_secs()
            ));
        }
        Ok(Ok(output)) => {
            return Err(format!(
                "git-ai-unavailable: private git-ai daemon unit {} failed to launch: {}",
                runtime.unit,
                command_detail(&output)
            ));
        }
        Ok(Err(error)) => {
            return Err(format!(
                "git-ai-unavailable: cannot launch private git-ai daemon unit {}: {error}",
                runtime.unit
            ));
        }
    };

    runtime.backend = backend;
    if let Err(reason) = ping_until(&runtime.control_socket, DAEMON_START_TIMEOUT).await {
        runtime.shutdown().await;
        return Err(reason);
    }
    Ok(runtime)
}

pub(crate) fn runtime_failure(
    execution: &GitAiExecution,
    preflight: &Preflight,
    reason: String,
) -> Result<Preflight, String> {
    if execution.config.mode == GitAiMode::Required {
        return Err(reason);
    }
    Ok(Preflight::Failed {
        provider_version: preflight_version(preflight).to_owned(),
        status: if reason.starts_with("git-ai-unavailable:") {
            AuthorshipStatus::Unavailable
        } else {
            AuthorshipStatus::Error
        },
        reason,
    })
}

pub(crate) async fn bind(
    execution: &GitAiExecution,
    preflight: &Preflight,
    completion: &SemanticCompletion,
    worktree: Option<&Path>,
    runtime: Option<&PrivateDaemon>,
) -> Binding {
    bind_inner(
        execution,
        preflight,
        completion,
        worktree,
        &CommandContext::default(),
        runtime,
        false,
    )
    .await
}

#[cfg(test)]
async fn bind_with_context(
    execution: &GitAiExecution,
    preflight: &Preflight,
    completion: &SemanticCompletion,
    worktree: Option<&Path>,
    context: &CommandContext,
) -> Binding {
    bind_inner(
        execution, preflight, completion, worktree, context, None, true,
    )
    .await
}

async fn bind_inner(
    execution: &GitAiExecution,
    preflight: &Preflight,
    completion: &SemanticCompletion,
    worktree: Option<&Path>,
    context: &CommandContext,
    runtime: Option<&PrivateDaemon>,
    allow_legacy_global: bool,
) -> Binding {
    let Some(raw_revision) = completion
        .gates
        .artifact
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|artifact| artifact.get("resultRevision"))
    else {
        return Binding::default();
    };
    let Some(revision) = raw_revision.as_str() else {
        return failure_without_binding(
            execution,
            "git-ai-error: resultRevision must be a string".to_owned(),
        );
    };
    if !git_oid_shape(revision) {
        return failure_without_binding(
            execution,
            "git-ai-error: resultRevision must be a 40- or 64-character lowercase Git object ID"
                .to_owned(),
        );
    }
    let revision = revision.to_owned();
    let Some(worktree) = worktree else {
        return finish(
            execution,
            revision,
            authorship(
                preflight_version(preflight),
                AuthorshipStatus::Error,
                None,
                None,
                Some("git-ai-error: resultRevision has no execution worktree".to_owned()),
            ),
        );
    };
    let Preflight::Available {
        provider_version, ..
    } = preflight
    else {
        let Preflight::Failed {
            provider_version,
            status,
            reason,
        } = preflight
        else {
            unreachable!()
        };
        return finish(
            execution,
            revision,
            authorship(provider_version, *status, None, None, Some(reason.clone())),
        );
    };

    let settlement = if let Some(runtime) = runtime {
        match runtime
            .settle_family(
                worktree,
                Duration::from_secs(execution.config.await_timeout_sec),
            )
            .await
        {
            Ok(()) => Ok(()),
            Err(reason)
                if provider_version != FAMILY_ADAPTER_VERSION
                    && execution.config.global_await_ok =>
            {
                let _unsupported_version = reason;
                runtime
                    .settle_global_fallback(worktree, execution.config.await_timeout_sec)
                    .await
            }
            Err(reason) => Err(reason),
        }
    } else if allow_legacy_global && execution.config.global_await_ok {
        settle_legacy_global(execution, worktree, context).await
    } else if allow_legacy_global {
        Err(format!(
            "git-ai-error: git-ai {provider_version} exposes no repository-family-scoped await and globalAwaitOk is false"
        ))
    } else {
        Err(format!(
            "git-ai-error: git-ai {provider_version} has no isolated repository-family settlement runtime"
        ))
    };
    if let Err(reason) = settlement {
        let status = if reason.starts_with("git-ai-unavailable:") {
            AuthorshipStatus::Unavailable
        } else {
            AuthorshipStatus::Error
        };
        return finish(
            execution,
            revision,
            authorship(provider_version, status, None, None, Some(reason)),
        );
    }

    let resolved = match git_output(
        ["rev-parse", "--verify", revision.as_str()],
        worktree,
        context,
    )
    .await
    {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .trim()
            .to_ascii_lowercase(),
        Ok(output) => {
            return finish(
                execution,
                revision.clone(),
                authorship(
                    provider_version,
                    AuthorshipStatus::Error,
                    None,
                    None,
                    Some(format!(
                        "git-ai-error: resultRevision {revision} does not resolve in {}: {}",
                        worktree.display(),
                        command_detail(&output)
                    )),
                ),
            );
        }
        Err(error) => {
            return finish(
                execution,
                revision.clone(),
                authorship(
                    provider_version,
                    AuthorshipStatus::Error,
                    None,
                    None,
                    Some(format!(
                        "git-ai-error: cannot resolve resultRevision {revision} in {}: {error}",
                        worktree.display()
                    )),
                ),
            );
        }
    };
    if resolved != revision {
        return finish(
            execution,
            revision.clone(),
            authorship(
                provider_version,
                AuthorshipStatus::Error,
                None,
                None,
                Some(format!(
                    "git-ai-error: resultRevision {revision} resolved as unexpected object {resolved}"
                )),
            ),
        );
    }

    let note = match git_output(
        ["notes", "--ref", NOTE_REF, "show", revision.as_str()],
        worktree,
        context,
    )
    .await
    {
        Ok(output) if output.status.success() => output.stdout,
        Ok(output) => {
            return finish(
                execution,
                revision.clone(),
                authorship(
                    provider_version,
                    AuthorshipStatus::MissingNote,
                    None,
                    None,
                    Some(format!(
                        "git-ai-missing-note: {NOTE_REF} has no note for {revision}: {}",
                        command_detail(&output)
                    )),
                ),
            );
        }
        Err(error) => {
            return finish(
                execution,
                revision.clone(),
                authorship(
                    provider_version,
                    AuthorshipStatus::Error,
                    None,
                    None,
                    Some(format!("git-ai-error: cannot read {NOTE_REF}: {error}")),
                ),
            );
        }
    };
    let note_hash = format!("sha256:{:x}", Sha256::digest(&note));
    let notes_ref_target = match git_output(["rev-parse", NOTE_REF], worktree, context).await {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        }
        Ok(output) => {
            return finish(
                execution,
                revision.clone(),
                authorship(
                    provider_version,
                    AuthorshipStatus::Error,
                    None,
                    Some(note_hash),
                    Some(format!(
                        "git-ai-error: cannot resolve {NOTE_REF}: {}",
                        command_detail(&output)
                    )),
                ),
            );
        }
        Err(error) => {
            return finish(
                execution,
                revision.clone(),
                authorship(
                    provider_version,
                    AuthorshipStatus::Error,
                    None,
                    Some(note_hash),
                    Some(format!("git-ai-error: cannot resolve {NOTE_REF}: {error}")),
                ),
            );
        }
    };
    if !git_oid_shape(&notes_ref_target) {
        return finish(
            execution,
            revision.clone(),
            authorship(
                provider_version,
                AuthorshipStatus::Error,
                None,
                Some(note_hash),
                Some(format!(
                    "git-ai-error: {NOTE_REF} resolved to invalid object ID {notes_ref_target:?}"
                )),
            ),
        );
    }

    match note_matches(&note, execution) {
        Ok(sessions) => finish_with_sessions(
            execution,
            revision,
            authorship(
                provider_version,
                AuthorshipStatus::Bound,
                Some(notes_ref_target),
                Some(note_hash),
                None,
            ),
            sessions,
        ),
        Err(NoteFailure::Mismatch { reason, sessions }) => finish_with_sessions(
            execution,
            revision,
            authorship(
                provider_version,
                AuthorshipStatus::Mismatch,
                Some(notes_ref_target),
                Some(note_hash),
                Some(format!("git-ai-mismatch: {reason}")),
            ),
            sessions,
        ),
        Err(NoteFailure::Malformed(reason)) => finish(
            execution,
            revision,
            authorship(
                provider_version,
                AuthorshipStatus::Error,
                Some(notes_ref_target),
                Some(note_hash),
                Some(format!("git-ai-error: malformed authorship note: {reason}")),
            ),
        ),
    }
}

async fn settle_legacy_global(
    execution: &GitAiExecution,
    worktree: &Path,
    context: &CommandContext,
) -> Result<(), String> {
    let timeout = execution.config.await_timeout_sec.to_string();
    let await_result = tokio::time::timeout(
        Duration::from_secs(execution.config.await_timeout_sec.saturating_add(1)),
        command_output(
            PROGRAM,
            ["await", "--timeout", timeout.as_str()],
            Some(worktree),
            context,
        ),
    )
    .await;
    match await_result {
        Err(_) => Err(format!(
            "git-ai-error: git-ai await timed out after {} seconds",
            execution.config.await_timeout_sec
        )),
        Ok(Ok(output)) if output.status.success() => Ok(()),
        Ok(Ok(output)) => Err(format!(
            "git-ai-unavailable: git-ai await failed: {}",
            command_detail(&output)
        )),
        Ok(Err(error)) => Err(format!(
            "git-ai-unavailable: cannot execute git-ai await: {error}"
        )),
    }
}

fn finish(execution: &GitAiExecution, revision: String, authorship: Authorship) -> Binding {
    finish_with_optional_sessions(execution, revision, authorship, None)
}

fn finish_with_sessions(
    execution: &GitAiExecution,
    revision: String,
    authorship: Authorship,
    sessions: Vec<AuthorshipSession>,
) -> Binding {
    finish_with_optional_sessions(execution, revision, authorship, Some(sessions))
}

fn finish_with_optional_sessions(
    execution: &GitAiExecution,
    revision: String,
    authorship: Authorship,
    authorship_sessions: Option<Vec<AuthorshipSession>>,
) -> Binding {
    let required_failure = (execution.config.mode == GitAiMode::Required
        && authorship.status != AuthorshipStatus::Bound)
        .then(|| {
            authorship
                .reason
                .clone()
                .unwrap_or_else(|| "git-ai-error: authorship binding failed".to_owned())
        });
    Binding {
        result_revision: Some(revision),
        authorship: Some(authorship),
        authorship_sessions: authorship_sessions.filter(|sessions| !sessions.is_empty()),
        required_failure,
    }
}

fn failure_without_binding(execution: &GitAiExecution, reason: String) -> Binding {
    Binding {
        result_revision: None,
        authorship: None,
        authorship_sessions: None,
        required_failure: (execution.config.mode == GitAiMode::Required).then_some(reason),
    }
}

fn authorship(
    provider_version: &str,
    status: AuthorshipStatus,
    notes_ref_target: Option<String>,
    note_content_sha256: Option<String>,
    reason: Option<String>,
) -> Authorship {
    Authorship {
        provider: "git-ai".to_owned(),
        provider_version: provider_version.to_owned(),
        note_ref: NOTE_REF.to_owned(),
        status,
        notes_ref_target,
        note_content_sha256,
        reason,
    }
}

fn preflight_version(preflight: &Preflight) -> &str {
    match preflight {
        Preflight::Available {
            provider_version, ..
        }
        | Preflight::Failed {
            provider_version, ..
        } => provider_version,
    }
}

#[derive(Debug)]
enum NoteFailure {
    Mismatch {
        reason: String,
        sessions: Vec<AuthorshipSession>,
    },
    Malformed(String),
}

fn note_matches(
    note: &[u8],
    execution: &GitAiExecution,
) -> Result<Vec<AuthorshipSession>, NoteFailure> {
    let divider = note
        .windows(NOTE_DIVIDER.len())
        .rposition(|window| window == NOTE_DIVIDER)
        .ok_or_else(|| NoteFailure::Malformed("metadata divider is absent".to_owned()))?;
    let metadata: Value = serde_json::from_slice(&note[divider + NOTE_DIVIDER.len()..])
        .map_err(|error| NoteFailure::Malformed(format!("metadata JSON is invalid: {error}")))?;
    let object = metadata
        .as_object()
        .ok_or_else(|| NoteFailure::Malformed("metadata is not an object".to_owned()))?;
    if object.get("schema_version").and_then(Value::as_str) != Some("authorship/3.0.0") {
        return Err(NoteFailure::Malformed(
            "schema_version is not authorship/3.0.0".to_owned(),
        ));
    }
    if object
        .get("base_commit_sha")
        .and_then(Value::as_str)
        .is_none_or(|revision| !git_oid_shape(revision))
    {
        return Err(NoteFailure::Malformed(
            "base_commit_sha is not a full Git object ID".to_owned(),
        ));
    }
    if object
        .get("git_ai_version")
        .is_some_and(|version| version.as_str().is_none())
    {
        return Err(NoteFailure::Malformed(
            "git_ai_version is not a string".to_owned(),
        ));
    }
    let prompts = object
        .get("prompts")
        .and_then(Value::as_object)
        .ok_or_else(|| NoteFailure::Malformed("prompts is not an object".to_owned()))?;
    let sessions = object
        .get("sessions")
        .map(|value| {
            value
                .as_object()
                .ok_or_else(|| NoteFailure::Malformed("sessions is not an object".to_owned()))
        })
        .transpose()?
        .into_iter()
        .flat_map(|sessions| sessions.values());
    let mut correlated = false;
    let mut matched = false;
    let mut observations = Vec::new();
    for record in prompts.values().chain(sessions) {
        let Some(record) = record.as_object() else {
            return Err(NoteFailure::Malformed(
                "prompt/session record is not an object".to_owned(),
            ));
        };
        let Some(attributes) = record.get("custom_attributes").and_then(Value::as_object) else {
            continue;
        };
        if attributes.values().any(|value| value.as_str().is_none()) {
            return Err(NoteFailure::Malformed(
                "custom_attributes is not a string-to-string object".to_owned(),
            ));
        }
        if !execution.attributes.iter().all(|(name, expected)| {
            attributes.get(name).and_then(Value::as_str) == Some(expected.as_str())
        }) {
            continue;
        }
        correlated = true;
        let Some(agent) = record.get("agent_id").and_then(Value::as_object) else {
            return Err(NoteFailure::Malformed(
                "correlated prompt/session has no agent_id object".to_owned(),
            ));
        };
        for field in ["tool", "id", "model"] {
            if agent
                .get(field)
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
            {
                return Err(NoteFailure::Malformed(format!(
                    "correlated prompt/session agent_id.{field} is absent or empty"
                )));
            }
        }
        let observation = AuthorshipSession {
            tool: agent
                .get("tool")
                .and_then(Value::as_str)
                .expect("validated agent_id.tool")
                .to_owned(),
            id: agent
                .get("id")
                .and_then(Value::as_str)
                .expect("validated agent_id.id")
                .to_owned(),
            model: agent
                .get("model")
                .and_then(Value::as_str)
                .expect("validated agent_id.model")
                .to_owned(),
        };
        observation.validate().map_err(NoteFailure::Malformed)?;
        observations.push(observation);
        let session_matches = execution
            .expected_session
            .as_deref()
            .is_none_or(|expected| agent.get("id").and_then(Value::as_str) == Some(expected));
        let model_matches = execution
            .expected_model
            .as_deref()
            .is_none_or(|expected| agent.get("model").and_then(Value::as_str) == Some(expected));
        matched |= session_matches && model_matches;
    }
    observations.sort();
    observations.dedup();
    if observations.len() > 16 {
        return Err(NoteFailure::Malformed(
            "more than 16 correlated agent sessions are present".to_owned(),
        ));
    }
    if matched {
        return Ok(observations);
    }
    if correlated {
        Err(NoteFailure::Mismatch {
            reason: "Tally session/model differs from Git AI's correlated attribution".to_owned(),
            sessions: observations,
        })
    } else {
        Err(NoteFailure::Mismatch {
            reason: "no Git AI session or prompt carries the Tally correlation attributes"
                .to_owned(),
            sessions: observations,
        })
    }
}

fn git_oid_shape(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn unavailable_reason() -> String {
    format!(
        "git-ai-unavailable: git-ai is not on PATH on {}; tally.nix does not package git-ai — it is provisioned by the dotfiles layer; enable it there and redeploy",
        hostname()
    )
}

fn hostname() -> String {
    current_host_id()
        .ok()
        .unwrap_or_else(|| "unknown-host".to_owned())
}

fn resolve_program(program: &str, context: &CommandContext) -> Option<PathBuf> {
    let candidate = Path::new(program);
    if candidate.components().count() > 1 {
        return is_executable(candidate).then(|| candidate.to_owned());
    }
    let path = context
        .path
        .as_ref()
        .cloned()
        .or_else(|| std::env::var_os("PATH"))?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(program))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

pub(crate) fn private_daemon_paths(
    provider_program: &Path,
    provider_version: &str,
    state_dir: &Path,
    runtime_key: &str,
    systemctl: &Path,
) -> Result<PrivateDaemon, String> {
    if runtime_key.is_empty()
        || runtime_key.len() > 1024
        || runtime_key.chars().any(char::is_control)
    {
        return Err("git-ai-error: private daemon runtime identity is invalid".to_owned());
    }
    let digest = format!("{:x}", Sha256::digest(runtime_key.as_bytes()));
    let short = &digest[..24];
    let daemon_home = state_dir.join(PRIVATE_DAEMON_DIRECTORY).join(short);
    let private_home = daemon_home.join("home");
    let control_socket = daemon_home.join("control.sock");
    let trace_socket = daemon_home.join("trace.sock");
    for (label, path) in [("control", &control_socket), ("trace", &trace_socket)] {
        let bytes = path.as_os_str().as_encoded_bytes().len();
        if bytes >= 100 {
            return Err(format!(
                "git-ai-error: private daemon {label} socket path is {bytes} bytes; move Tally's state directory so it stays below the Unix socket limit"
            ));
        }
    }
    Ok(PrivateDaemon {
        provider_program: provider_program.to_owned(),
        provider_version: provider_version.to_owned(),
        daemon_home,
        private_home,
        control_socket,
        trace_socket,
        unit: format!("{PRIVATE_DAEMON_PREFIX}-{short}.service"),
        systemctl: systemctl.to_owned(),
        backend: DaemonBackend::Systemd,
        armed: true,
    })
}

fn prepare_private_directory(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|error| {
        format!(
            "git-ai-error: cannot create private daemon directory {}: {error}",
            path.display()
        )
    })?;
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        format!(
            "git-ai-error: cannot inspect private daemon directory {}: {error}",
            path.display()
        )
    })?;
    if !metadata.file_type().is_dir() {
        return Err(format!(
            "git-ai-error: private daemon path {} is not a real directory",
            path.display()
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
        format!(
            "git-ai-error: cannot protect private daemon directory {}: {error}",
            path.display()
        )
    })
}

fn quote_systemd_word(word: &OsStr) -> Result<String, String> {
    let word = word
        .to_str()
        .ok_or_else(|| "git-ai-error: private daemon systemd path is not valid UTF-8".to_owned())?;
    if word.chars().any(char::is_control) || word.contains('%') {
        return Err(
            "git-ai-error: private daemon systemd path contains a control or specifier character"
                .to_owned(),
        );
    }
    let mut quoted = String::with_capacity(word.len() + 2);
    quoted.push('"');
    for character in word.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    Ok(quoted)
}

fn push_systemd_property(args: &mut Vec<OsString>, property: impl Into<OsString>) {
    args.push("--property".into());
    args.push(property.into());
}

async fn start_direct_daemon(
    runtime: &PrivateDaemon,
    execution: &GitAiExecution,
    path: &str,
) -> Result<(), String> {
    let attributes = execution
        .attributes_json()
        .map_err(|error| format!("git-ai-error: cannot encode custom attributes: {error}"))?;
    let mut context = runtime.command_context();
    context.path = Some(path.into());
    context
        .environment
        .insert("GIT_AI_CUSTOM_ATTRIBUTES".to_owned(), attributes);
    let output = tokio::time::timeout(
        DAEMON_START_TIMEOUT,
        command_output(&runtime.provider_program, ["bg", "start"], None, &context),
    )
    .await
    .map_err(|_| {
        format!(
            "git-ai-error: direct private git-ai daemon launcher timed out after {} seconds",
            DAEMON_START_TIMEOUT.as_secs()
        )
    })?
    .map_err(|error| {
        format!("git-ai-unavailable: cannot launch direct private git-ai daemon: {error}")
    })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "git-ai-unavailable: direct private git-ai daemon failed to launch: {}",
            command_detail(&output)
        ))
    }
}

async fn ping_until(socket: &Path, timeout: Duration) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if send_control_request(socket, &ControlRequest::Ping, Duration::from_millis(250))
            .await
            .is_ok()
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "git-ai-error: private git-ai daemon did not become ready at {} within {} seconds",
                socket.display(),
                timeout.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn send_control_request(
    socket: &Path,
    request: &ControlRequest,
    timeout: Duration,
) -> Result<(), String> {
    let operation = async {
        let mut stream = UnixStream::connect(socket).await.map_err(|error| {
            format!(
                "git-ai-unavailable: cannot connect to private git-ai control socket {}: {error}",
                socket.display()
            )
        })?;
        let mut body = serde_json::to_vec(request)
            .map_err(|error| format!("git-ai-error: cannot encode control request: {error}"))?;
        body.push(b'\n');
        stream.write_all(&body).await.map_err(|error| {
            format!("git-ai-unavailable: cannot write private git-ai control request: {error}")
        })?;
        let mut response = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            let count = stream.read(&mut chunk).await.map_err(|error| {
                format!("git-ai-unavailable: cannot read private git-ai control response: {error}")
            })?;
            if count == 0 {
                return Err(
                    "git-ai-error: private git-ai control socket returned no complete response"
                        .to_owned(),
                );
            }
            let newline = chunk[..count].iter().position(|byte| *byte == b'\n');
            let accepted = newline.map_or(count, |index| index + 1);
            if response.len().saturating_add(accepted) > CONTROL_RESPONSE_LIMIT {
                return Err(format!(
                    "git-ai-error: private git-ai control response exceeds {CONTROL_RESPONSE_LIMIT} bytes"
                ));
            }
            response.extend_from_slice(&chunk[..accepted]);
            if newline.is_some() {
                break;
            }
        }
        let parsed: ControlResponse = serde_json::from_slice(
            response
                .strip_suffix(b"\n")
                .expect("response loop stops on a newline"),
        )
        .map_err(|error| {
            format!("git-ai-error: malformed private git-ai control response: {error}")
        })?;
        if parsed.ok {
            Ok(())
        } else {
            Err(format!(
                "git-ai-error: private git-ai control request was rejected: {}",
                parsed.error.unwrap_or_else(|| "no error detail".to_owned())
            ))
        }
    };
    tokio::time::timeout(timeout, operation)
        .await
        .map_err(|_| {
            format!(
                "git-ai-error: private git-ai control request timed out after {} seconds",
                timeout.as_secs()
            )
        })?
}

fn best_effort_sync_shutdown(socket: &Path) {
    let Ok(mut stream) = StdUnixStream::connect(socket) else {
        return;
    };
    let timeout = Some(Duration::from_millis(100));
    let _ = stream.set_write_timeout(timeout);
    let _ = stream.set_read_timeout(timeout);
    let _ = stream.write_all(b"{\"method\":\"shutdown\"}\n");
    let mut response = [0_u8; 256];
    let _ = stream.read(&mut response);
}

fn utf8_path<'a>(path: &'a Path, label: &str) -> Result<&'a str, String> {
    path.to_str()
        .ok_or_else(|| format!("git-ai-error: {label} is not valid UTF-8"))
}

async fn git_output<const N: usize>(
    args: [&str; N],
    worktree: &Path,
    context: &CommandContext,
) -> std::io::Result<std::process::Output> {
    command_output("git", args, Some(worktree), context).await
}

async fn command_output<I, S>(
    program: impl AsRef<OsStr>,
    args: I,
    cwd: Option<&Path>,
    context: &CommandContext,
) -> std::io::Result<std::process::Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new(program);
    command.kill_on_drop(true);
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    if let Some(path) = &context.path {
        command.env("PATH", path);
    }
    for (name, value) in &context.environment {
        command.env(name, value);
    }
    command.output().await
}

fn command_detail(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    if detail.is_empty() {
        format!("exit status {}", output.status)
    } else {
        detail
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;

    use super::*;
    use crate::completion::SemanticCompletion;

    fn run_git(repo: &Path, args: &[&str]) -> std::process::Output {
        run_git_with_environment(repo, args, &[])
    }

    fn run_git_with_environment(
        repo: &Path,
        args: &[&str],
        environment: &[(String, String)],
    ) -> std::process::Output {
        let mut command = std::process::Command::new("git");
        command.arg("-C").arg(repo).args(args);
        for (name, value) in environment {
            command.env(name, value);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn fixture_repo() -> (tempfile::TempDir, String) {
        let temp = tempfile::tempdir().unwrap();
        run_git(temp.path(), &["init", "-q"]);
        run_git(temp.path(), &["config", "user.name", "Tally Test"]);
        run_git(
            temp.path(),
            &["config", "user.email", "tally@example.invalid"],
        );
        fs::write(temp.path().join("result.txt"), "result\n").unwrap();
        run_git(temp.path(), &["add", "result.txt"]);
        run_git(temp.path(), &["commit", "-q", "-m", "result"]);
        let revision = String::from_utf8(run_git(temp.path(), &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_owned();
        (temp, revision)
    }

    fn stub_path(root: &Path) -> CommandContext {
        stub_path_with_await(root, "exit 0")
    }

    fn stub_path_with_await(root: &Path, await_command: &str) -> CommandContext {
        let bin = root.join("bin");
        fs::create_dir(&bin).unwrap();
        let program = bin.join("git-ai");
        fs::write(
            &program,
            format!(
                "#!/bin/sh\ncase \"$1\" in\n  --version) printf '1.6.17\\n' ;;\n  await) {await_command} ;;\n  *) exit 64 ;;\nesac\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        let mut paths = vec![bin];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        CommandContext {
            path: Some(std::env::join_paths(paths).unwrap()),
            environment: BTreeMap::new(),
        }
    }

    fn execution(mode: GitAiMode) -> GitAiExecution {
        GitAiExecution {
            config: GitAiConfig {
                enable: true,
                mode,
                await_timeout_sec: 17,
                global_await_ok: true,
            },
            attributes: BTreeMap::from([
                ("adapter".to_owned(), "codex".to_owned()),
                ("attempt".to_owned(), "2".to_owned()),
                ("leaseEpoch".to_owned(), "9".to_owned()),
                (
                    "taskUuid".to_owned(),
                    "00000000-0000-4000-8000-000000000053".to_owned(),
                ),
            ]),
            expected_session: Some("session-53".to_owned()),
            expected_model: Some("gpt-5".to_owned()),
        }
    }

    fn completion(revision: &str, gate_status: &str) -> SemanticCompletion {
        serde_json::from_value(json!({
            "schemaVersion": 1,
            "execution": {
                "status": "success",
                "exitCode": 0,
                "reason": "process exited with code 0"
            },
            "gates": {
                "status": gate_status,
                "artifact": {"resultRevision": revision},
                "gates": []
            },
            "acceptance": {
                "status": if gate_status == "pass" { "accepted" } else { "rejected" },
                "policy": "execution-and-gates",
                "reason": "fixture"
            }
        }))
        .unwrap()
    }

    fn note(attributes: &BTreeMap<String, String>, session: &str, model: &str) -> Vec<u8> {
        format!(
            "result.txt\n  s_0123456789abcd::t_0123456789abcd 1\n---\n{}\n",
            serde_json::to_string_pretty(&json!({
                "schema_version": "authorship/3.0.0",
                "git_ai_version": "1.6.17",
                "base_commit_sha": "0000000000000000000000000000000000000000",
                "prompts": {},
                "sessions": {
                    "s_0123456789abcd": {
                        "agent_id": {
                            "tool": "codex",
                            "id": session,
                            "model": model
                        },
                        "custom_attributes": attributes
                    }
                }
            }))
            .unwrap()
        )
        .into_bytes()
    }

    fn install_note(repo: &Path, bytes: &[u8]) {
        let path = repo.join("authorship-note");
        fs::write(&path, bytes).unwrap();
        run_git(
            repo,
            &[
                "notes",
                "--ref",
                NOTE_REF,
                "add",
                "-F",
                path.to_str().unwrap(),
                "HEAD",
            ],
        );
        assert_eq!(
            run_git(repo, &["notes", "--ref", NOTE_REF, "show", "HEAD"]).stdout,
            bytes
        );
    }

    async fn available(execution: &GitAiExecution, context: &CommandContext) -> Preflight {
        preflight_with_context(execution, context).await.unwrap()
    }

    fn control_server(
        socket: &Path,
        expected: Value,
        response: Vec<u8>,
        delay: Duration,
    ) -> tokio::task::JoinHandle<()> {
        let listener = UnixListener::bind(socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = tokio::io::BufReader::new(stream);
            let mut request = Vec::new();
            reader.read_until(b'\n', &mut request).await.unwrap();
            let request: Value = serde_json::from_slice(&request).unwrap();
            assert_eq!(request, expected);
            tokio::time::sleep(delay).await;
            let _ = reader.get_mut().write_all(&response).await;
        })
    }

    #[tokio::test]
    async fn advisory_happy_path_hashes_the_exact_note_bytes() {
        let (repo, revision) = fixture_repo();
        let execution = execution(GitAiMode::Advisory);
        let note = note(
            &execution.attributes,
            execution.expected_session.as_deref().unwrap(),
            execution.expected_model.as_deref().unwrap(),
        );
        install_note(repo.path(), &note);
        let context = stub_path(repo.path());
        let preflight = available(&execution, &context).await;
        let binding = bind_with_context(
            &execution,
            &preflight,
            &completion(&revision, "pass"),
            Some(repo.path()),
            &context,
        )
        .await;
        assert_eq!(binding.result_revision.as_deref(), Some(revision.as_str()));
        let authorship = binding.authorship.unwrap();
        assert_eq!(authorship.status, AuthorshipStatus::Bound);
        assert_eq!(authorship.provider_version, "1.6.17");
        assert_eq!(
            authorship.note_content_sha256,
            Some(format!("sha256:{:x}", Sha256::digest(&note)))
        );
        assert_eq!(
            authorship.notes_ref_target,
            Some(
                String::from_utf8(run_git(repo.path(), &["rev-parse", NOTE_REF]).stdout)
                    .unwrap()
                    .trim()
                    .to_owned()
            )
        );
        assert_eq!(
            binding.authorship_sessions,
            Some(vec![AuthorshipSession {
                tool: "codex".to_owned(),
                id: "session-53".to_owned(),
                model: "gpt-5".to_owned(),
            }])
        );
        assert_eq!(binding.required_failure, None);
    }

    #[tokio::test]
    async fn required_missing_binary_is_the_exact_typed_terminal_reason() {
        let execution = execution(GitAiMode::Required);
        let empty = tempfile::tempdir().unwrap();
        let context = CommandContext {
            path: Some(empty.path().as_os_str().to_owned()),
            environment: BTreeMap::new(),
        };
        let reason = preflight_with_context(&execution, &context)
            .await
            .unwrap_err();
        assert_eq!(reason, unavailable_reason());
        assert!(reason.starts_with("git-ai-unavailable:"));
        assert!(reason.contains("tally.nix does not package git-ai"));
        assert!(reason.contains("dotfiles layer"));
    }

    #[tokio::test]
    async fn advisory_missing_binary_runs_with_unavailable_authorship() {
        let execution = execution(GitAiMode::Advisory);
        let empty = tempfile::tempdir().unwrap();
        let context = CommandContext {
            path: Some(empty.path().as_os_str().to_owned()),
            environment: BTreeMap::new(),
        };
        let preflight = preflight_with_context(&execution, &context).await.unwrap();
        let binding = bind_with_context(
            &execution,
            &preflight,
            &completion("0000000000000000000000000000000000000000", "pass"),
            Some(empty.path()),
            &context,
        )
        .await;
        let authorship = binding.authorship.unwrap();
        assert_eq!(authorship.status, AuthorshipStatus::Unavailable);
        assert_eq!(authorship.reason, Some(unavailable_reason()));
        assert_eq!(binding.required_failure, None);
    }

    #[tokio::test]
    async fn required_missing_note_fails_the_binding() {
        let (repo, revision) = fixture_repo();
        let execution = execution(GitAiMode::Required);
        let context = stub_path(repo.path());
        let preflight = available(&execution, &context).await;
        let binding = bind_with_context(
            &execution,
            &preflight,
            &completion(&revision, "pass"),
            Some(repo.path()),
            &context,
        )
        .await;
        assert_eq!(
            binding.authorship.as_ref().map(|value| value.status),
            Some(AuthorshipStatus::MissingNote)
        );
        assert!(binding
            .required_failure
            .as_deref()
            .is_some_and(|reason| reason.starts_with("git-ai-missing-note:")));
    }

    #[tokio::test]
    async fn required_malformed_note_fails_with_a_typed_reason() {
        let (repo, revision) = fixture_repo();
        let execution = execution(GitAiMode::Required);
        install_note(repo.path(), b"not an authorship note\n");
        let context = stub_path(repo.path());
        let preflight = available(&execution, &context).await;
        let binding = bind_with_context(
            &execution,
            &preflight,
            &completion(&revision, "pass"),
            Some(repo.path()),
            &context,
        )
        .await;
        assert_eq!(
            binding.authorship.as_ref().map(|value| value.status),
            Some(AuthorshipStatus::Error)
        );
        assert!(binding
            .required_failure
            .as_deref()
            .is_some_and(|reason| reason.starts_with("git-ai-error: malformed authorship note:")));
    }

    #[tokio::test]
    async fn required_invalid_result_revision_fails_before_binding() {
        let execution = execution(GitAiMode::Required);
        let temp = tempfile::tempdir().unwrap();
        let context = stub_path(temp.path());
        let preflight = available(&execution, &context).await;
        let binding = bind_with_context(
            &execution,
            &preflight,
            &completion("HEAD", "pass"),
            None,
            &context,
        )
        .await;
        assert_eq!(binding.result_revision, None);
        assert_eq!(binding.authorship, None);
        assert!(binding
            .required_failure
            .as_deref()
            .is_some_and(|reason| reason.starts_with("git-ai-error: resultRevision")));
    }

    #[tokio::test]
    async fn required_settlement_timeout_fails_typed_and_kills_the_stub() {
        let (repo, revision) = fixture_repo();
        let mut execution = execution(GitAiMode::Required);
        execution.config.await_timeout_sec = 1;
        let context = stub_path_with_await(repo.path(), "exec sleep 30");
        let preflight = available(&execution, &context).await;
        let binding = bind_with_context(
            &execution,
            &preflight,
            &completion(&revision, "pass"),
            Some(repo.path()),
            &context,
        )
        .await;
        assert_eq!(
            binding.authorship.as_ref().map(|value| value.status),
            Some(AuthorshipStatus::Error)
        );
        assert_eq!(
            binding.required_failure.as_deref(),
            Some("git-ai-error: git-ai await timed out after 1 seconds")
        );
    }

    #[tokio::test]
    async fn session_or_model_mismatch_is_recorded_and_never_reconciled() {
        let (repo, revision) = fixture_repo();
        let execution = execution(GitAiMode::Advisory);
        let note = note(&execution.attributes, "another-session", "another-model");
        install_note(repo.path(), &note);
        let context = stub_path(repo.path());
        let preflight = available(&execution, &context).await;
        let binding = bind_with_context(
            &execution,
            &preflight,
            &completion(&revision, "pass"),
            Some(repo.path()),
            &context,
        )
        .await;
        let authorship = binding.authorship.unwrap();
        assert_eq!(authorship.status, AuthorshipStatus::Mismatch);
        assert!(authorship
            .reason
            .as_deref()
            .is_some_and(|reason| reason.starts_with("git-ai-mismatch:")));
        assert!(authorship.notes_ref_target.is_some());
        assert!(authorship.note_content_sha256.is_some());
        assert_eq!(
            binding.authorship_sessions,
            Some(vec![AuthorshipSession {
                tool: "codex".to_owned(),
                id: "another-session".to_owned(),
                model: "another-model".to_owned(),
            }])
        );
        assert_eq!(binding.required_failure, None);
    }

    #[tokio::test]
    async fn global_await_requires_an_explicit_isolated_host_opt_in() {
        let (repo, revision) = fixture_repo();
        let mut execution = execution(GitAiMode::Required);
        execution.config.global_await_ok = false;
        let context = stub_path(repo.path());
        let preflight = available(&execution, &context).await;
        let binding = bind_with_context(
            &execution,
            &preflight,
            &completion(&revision, "pass"),
            Some(repo.path()),
            &context,
        )
        .await;
        assert_eq!(
            binding.authorship.as_ref().map(|value| value.status),
            Some(AuthorshipStatus::Error)
        );
        assert!(binding
            .required_failure
            .as_deref()
            .is_some_and(|reason| reason.contains("globalAwaitOk is false")));
    }

    #[tokio::test]
    async fn version_pinned_family_adapter_sends_the_exact_linked_worktree_request() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("control.sock");
        let worktree = "/srv/repo linked/agents/worktree-53";
        let server = control_server(
            &socket,
            json!({
                "method": "sync.family",
                "params": {"repo_working_dir": worktree}
            }),
            br#"{"ok":true,"data":{"status":"settled"}}"#.iter().copied().chain([b'\n']).collect(),
            Duration::ZERO,
        );
        send_control_request(
            &socket,
            &ControlRequest::SyncFamily {
                repo_working_dir: worktree.to_owned(),
            },
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_stalled_repository_family_does_not_delay_an_unrelated_family() {
        let temp = tempfile::tempdir().unwrap();
        let stalled_socket = temp.path().join("stalled.sock");
        let ready_socket = temp.path().join("ready.sock");
        let stalled_server = control_server(
            &stalled_socket,
            json!({
                "method": "sync.family",
                "params": {"repo_working_dir": "/repos/stalled"}
            }),
            b"{\"ok\":true}\n".to_vec(),
            Duration::from_millis(300),
        );
        let ready_server = control_server(
            &ready_socket,
            json!({
                "method": "sync.family",
                "params": {"repo_working_dir": "/repos/ready"}
            }),
            b"{\"ok\":true}\n".to_vec(),
            Duration::ZERO,
        );
        let stalled_request = ControlRequest::SyncFamily {
            repo_working_dir: "/repos/stalled".to_owned(),
        };
        let ready_request = ControlRequest::SyncFamily {
            repo_working_dir: "/repos/ready".to_owned(),
        };
        let stalled =
            send_control_request(&stalled_socket, &stalled_request, Duration::from_millis(50));
        let ready = send_control_request(&ready_socket, &ready_request, Duration::from_secs(1));
        let (stalled, ready) = tokio::join!(stalled, ready);
        assert!(stalled.unwrap_err().contains("control request timed out"));
        ready.unwrap();
        stalled_server.await.unwrap();
        ready_server.await.unwrap();
    }

    #[tokio::test]
    async fn family_adapter_rejects_oversized_and_negative_responses() {
        let temp = tempfile::tempdir().unwrap();
        let oversized_socket = temp.path().join("oversized.sock");
        let rejected_socket = temp.path().join("rejected.sock");
        let mut oversized = vec![b'x'; CONTROL_RESPONSE_LIMIT + 1];
        oversized.push(b'\n');
        let oversized_server = control_server(
            &oversized_socket,
            json!({"method": "ping"}),
            oversized,
            Duration::ZERO,
        );
        let rejected_server = control_server(
            &rejected_socket,
            json!({"method": "ping"}),
            b"{\"ok\":false,\"error\":\"family queue unavailable\"}\n".to_vec(),
            Duration::ZERO,
        );
        assert!(send_control_request(
            &oversized_socket,
            &ControlRequest::Ping,
            Duration::from_secs(1)
        )
        .await
        .unwrap_err()
        .contains("exceeds 65536 bytes"));
        assert!(send_control_request(
            &rejected_socket,
            &ControlRequest::Ping,
            Duration::from_secs(1)
        )
        .await
        .unwrap_err()
        .contains("family queue unavailable"));
        oversized_server.await.unwrap();
        rejected_server.await.unwrap();
    }

    #[test]
    fn private_runtime_identity_is_deterministic_and_routes_trace2() {
        let temp = tempfile::tempdir().unwrap();
        let mut runtime = private_daemon_paths(
            Path::new("/opt/dotfiles/bin/git-ai"),
            FAMILY_ADAPTER_VERSION,
            temp.path(),
            "task-53:2:9",
            Path::new("/run/current-system/sw/bin/systemctl"),
        )
        .unwrap();
        runtime.armed = false;
        let environment = runtime
            .child_environment()
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            environment["GIT_TRACE2_EVENT"],
            format!("af_unix:stream:{}", runtime.trace_socket.to_string_lossy())
        );
        assert_eq!(
            environment["GIT_AI_DAEMON_CONTROL_SOCKET"],
            runtime.control_socket.to_string_lossy()
        );
        assert!(runtime.unit.starts_with("tally-git-ai-"));
        assert!(runtime.unit.ends_with(".service"));
        assert!(runtime.control_socket.starts_with(temp.path()));
    }

    fn checkpoint_fixture(
        runtime: &PrivateDaemon,
        execution: &GitAiExecution,
        worktree: &Path,
        preset: &str,
        hook: Value,
    ) {
        let mut environment = runtime.child_environment();
        environment.push((
            "GIT_AI_CUSTOM_ATTRIBUTES".to_owned(),
            execution.attributes_json().unwrap(),
        ));
        let mut command = std::process::Command::new(&runtime.provider_program);
        command.current_dir(worktree).args([
            "checkpoint",
            preset,
            "--hook-input",
            &hook.to_string(),
        ]);
        for (name, value) in environment {
            command.env(name, value);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "git-ai checkpoint {preset}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    async fn exercise_fleet_agent(
        root: &Path,
        preset: &str,
        session: &str,
        model: &str,
        linked_worktree: bool,
        amend: bool,
    ) -> (String, Vec<AuthorshipSession>) {
        let repository = root.join(format!("{preset}-repository"));
        let linked = root.join(format!("{preset}-linked"));
        fs::create_dir(&repository).unwrap();
        run_git(&repository, &["init", "-q"]);
        run_git(&repository, &["config", "user.name", "Tally Fleet Test"]);
        run_git(
            &repository,
            &["config", "user.email", "tally@example.invalid"],
        );
        fs::write(repository.join("agent.txt"), "initial\n").unwrap();
        run_git(&repository, &["add", "agent.txt"]);
        run_git(&repository, &["commit", "-q", "-m", "initial"]);
        let worktree = if linked_worktree {
            run_git(
                &repository,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    &format!("{preset}-linked"),
                    linked.to_str().unwrap(),
                    "HEAD",
                ],
            );
            linked
        } else {
            repository.clone()
        };

        let mut execution = execution(GitAiMode::Required);
        execution.attributes.insert(
            "taskUuid".to_owned(),
            format!("00000000-0000-4000-8000-{:0>12}", preset.len()),
        );
        execution
            .attributes
            .insert("adapter".to_owned(), preset.to_owned());
        execution.expected_session = Some(session.to_owned());
        execution.expected_model = Some(model.to_owned());
        let preflight = preflight(&execution).await.unwrap();
        let git_directories = String::from_utf8(
            run_git(
                &worktree,
                &[
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-dir",
                    "--git-common-dir",
                ],
            )
            .stdout,
        )
        .unwrap()
        .lines()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
        let runtime = start_private_daemon(
            &execution,
            &preflight,
            PrivateDaemonLaunch {
                state_dir: &root.join(format!("{preset}-state")),
                runtime_key: &format!("fleet-real:{preset}:{session}"),
                worktree: &worktree,
                repository_write_paths: &git_directories,
                systemd_run: Path::new("systemd-run"),
                systemctl: Path::new("systemctl"),
                allow_direct_fallback: false,
            },
        )
        .await
        .unwrap();

        let transcript = root.join(format!("{preset}-transcript.jsonl"));
        fs::write(
            &transcript,
            format!(
                "{}\n",
                json!({"type": "assistant", "message": {"model": model, "content": []}})
            ),
        )
        .unwrap();
        let file = worktree.join("agent.txt");
        fs::write(&file, format!("{preset} authored\n")).unwrap();
        let hook = if preset == "codex" {
            json!({
                "cwd": worktree,
                "hook_event_name": "PostToolUse",
                "tool_name": "apply_patch",
                "session_id": session,
                "tool_use_id": "edit-1",
                "model": model,
                "tool_input": {"file_path": file}
            })
        } else {
            json!({
                "cwd": worktree,
                "transcript_path": transcript,
                "hook_event_name": "PostToolUse",
                "tool_name": "Write",
                "session_id": session,
                "tool_use_id": "edit-1",
                "tool_input": {"file_path": file}
            })
        };
        checkpoint_fixture(&runtime, &execution, &worktree, preset, hook);
        let mut child_environment = runtime.child_environment();
        child_environment.push((
            "GIT_AI_CUSTOM_ATTRIBUTES".to_owned(),
            execution.attributes_json().unwrap(),
        ));
        run_git_with_environment(&worktree, &["add", "agent.txt"], &child_environment);
        run_git_with_environment(
            &worktree,
            &["commit", "-q", "-m", &format!("{preset} authored")],
            &child_environment,
        );

        if amend {
            fs::write(&file, format!("{preset} authored and amended\n")).unwrap();
            let hook = json!({
                "cwd": worktree,
                "transcript_path": transcript,
                "hook_event_name": "PostToolUse",
                "tool_name": "Edit",
                "session_id": session,
                "tool_use_id": "edit-2",
                "tool_input": {"file_path": file}
            });
            checkpoint_fixture(&runtime, &execution, &worktree, preset, hook);
            run_git_with_environment(&worktree, &["add", "agent.txt"], &child_environment);
            run_git_with_environment(
                &worktree,
                &["commit", "-q", "--amend", "--no-edit"],
                &child_environment,
            );
        }

        runtime
            .settle_family(&worktree, Duration::from_secs(20))
            .await
            .unwrap();
        let revision = String::from_utf8(run_git(&worktree, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_owned();
        let note = run_git(
            &worktree,
            &["notes", "--ref", NOTE_REF, "show", revision.as_str()],
        )
        .stdout;
        let observations = note_matches(&note, &execution).unwrap();
        runtime.shutdown().await;
        (revision, observations)
    }

    #[tokio::test]
    async fn fleet_real_codex_claude_linked_amend_and_family_isolation() {
        if std::env::var("TALLY_TEST_GIT_AI_FLEET").as_deref() != Ok("1") {
            return;
        }
        let runtime_parent = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .expect("fleet-real Git AI test requires XDG_RUNTIME_DIR");
        let temp = tempfile::Builder::new()
            .prefix("tally-git-ai-")
            .tempdir_in(runtime_parent)
            .unwrap();
        let codex = exercise_fleet_agent(
            temp.path(),
            "codex",
            "codex-fleet-session-53",
            "gpt-5",
            false,
            false,
        );
        let claude = exercise_fleet_agent(
            temp.path(),
            "claude",
            "claude-fleet-session-53",
            "claude-opus-4-6",
            true,
            true,
        );
        let ((codex_revision, codex_sessions), (claude_revision, claude_sessions)) =
            tokio::join!(codex, claude);
        assert_ne!(codex_revision, claude_revision);
        assert_eq!(
            codex_sessions,
            vec![AuthorshipSession {
                tool: "codex".to_owned(),
                id: "codex-fleet-session-53".to_owned(),
                model: "gpt-5".to_owned(),
            }]
        );
        assert_eq!(
            claude_sessions,
            vec![AuthorshipSession {
                tool: "claude".to_owned(),
                id: "claude-fleet-session-53".to_owned(),
                model: "claude-opus-4-6".to_owned(),
            }]
        );
    }
}
