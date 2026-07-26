use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::process::Command;

use crate::completion::SemanticCompletion;
use crate::config::{GitAiConfig, GitAiMode};
use crate::witness::{current_host_id, Authorship, AuthorshipStatus};

const PROGRAM: &str = "git-ai";
const NOTE_REF: &str = "refs/notes/ai";
const NOTE_DIVIDER: &[u8] = b"\n---\n";

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
    pub required_failure: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct CommandContext {
    path: Option<OsString>,
}

pub(crate) async fn preflight(execution: &GitAiExecution) -> Result<Preflight, String> {
    preflight_with_context(execution, &CommandContext::default()).await
}

async fn preflight_with_context(
    execution: &GitAiExecution,
    context: &CommandContext,
) -> Result<Preflight, String> {
    let output = command_output(PROGRAM, ["--version"], None, context).await;
    let failure = match output {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if !version.is_empty() && version.len() <= 256 && !version.chars().any(char::is_control)
            {
                return Ok(Preflight::Available {
                    provider_version: version,
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

pub(crate) async fn bind(
    execution: &GitAiExecution,
    preflight: &Preflight,
    completion: &SemanticCompletion,
    worktree: Option<&Path>,
) -> Binding {
    bind_with_context(
        execution,
        preflight,
        completion,
        worktree,
        &CommandContext::default(),
    )
    .await
}

async fn bind_with_context(
    execution: &GitAiExecution,
    preflight: &Preflight,
    completion: &SemanticCompletion,
    worktree: Option<&Path>,
    context: &CommandContext,
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
    let Preflight::Available { provider_version } = preflight else {
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

    if !execution.config.global_await_ok {
        return finish(
            execution,
            revision,
            authorship(
                provider_version,
                AuthorshipStatus::Error,
                None,
                None,
                Some(format!(
                    "git-ai-error: git-ai {provider_version} exposes no repository-family-scoped await and globalAwaitOk is false"
                )),
            ),
        );
    }
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
        Err(_) => {
            return finish(
                execution,
                revision,
                authorship(
                    provider_version,
                    AuthorshipStatus::Error,
                    None,
                    None,
                    Some(format!(
                        "git-ai-error: git-ai await timed out after {} seconds",
                        execution.config.await_timeout_sec
                    )),
                ),
            );
        }
        Ok(Ok(output)) if output.status.success() => {}
        Ok(Ok(output)) => {
            return finish(
                execution,
                revision,
                authorship(
                    provider_version,
                    AuthorshipStatus::Unavailable,
                    None,
                    None,
                    Some(format!(
                        "git-ai-unavailable: git-ai await failed: {}",
                        command_detail(&output)
                    )),
                ),
            );
        }
        Ok(Err(error)) => {
            return finish(
                execution,
                revision,
                authorship(
                    provider_version,
                    AuthorshipStatus::Unavailable,
                    None,
                    None,
                    Some(format!(
                        "git-ai-unavailable: cannot execute git-ai await: {error}"
                    )),
                ),
            );
        }
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
        Ok(()) => finish(
            execution,
            revision,
            authorship(
                provider_version,
                AuthorshipStatus::Bound,
                Some(notes_ref_target),
                Some(note_hash),
                None,
            ),
        ),
        Err(NoteFailure::Mismatch(reason)) => finish(
            execution,
            revision,
            authorship(
                provider_version,
                AuthorshipStatus::Mismatch,
                Some(notes_ref_target),
                Some(note_hash),
                Some(format!("git-ai-mismatch: {reason}")),
            ),
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

fn finish(execution: &GitAiExecution, revision: String, authorship: Authorship) -> Binding {
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
        required_failure,
    }
}

fn failure_without_binding(execution: &GitAiExecution, reason: String) -> Binding {
    Binding {
        result_revision: None,
        authorship: None,
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
        Preflight::Available { provider_version }
        | Preflight::Failed {
            provider_version, ..
        } => provider_version,
    }
}

enum NoteFailure {
    Mismatch(String),
    Malformed(String),
}

fn note_matches(note: &[u8], execution: &GitAiExecution) -> Result<(), NoteFailure> {
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
        let session_matches = execution
            .expected_session
            .as_deref()
            .is_none_or(|expected| agent.get("id").and_then(Value::as_str) == Some(expected));
        let model_matches = execution
            .expected_model
            .as_deref()
            .is_none_or(|expected| agent.get("model").and_then(Value::as_str) == Some(expected));
        if session_matches && model_matches {
            return Ok(());
        }
    }
    if correlated {
        Err(NoteFailure::Mismatch(
            "Tally session/model differs from Git AI's correlated attribution".to_owned(),
        ))
    } else {
        Err(NoteFailure::Mismatch(
            "no Git AI session or prompt carries the Tally correlation attributes".to_owned(),
        ))
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

    use super::*;
    use crate::completion::SemanticCompletion;

    fn run_git(repo: &Path, args: &[&str]) -> std::process::Output {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
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
        assert_eq!(binding.required_failure, None);
    }

    #[tokio::test]
    async fn required_missing_binary_is_the_exact_typed_terminal_reason() {
        let execution = execution(GitAiMode::Required);
        let empty = tempfile::tempdir().unwrap();
        let context = CommandContext {
            path: Some(empty.path().as_os_str().to_owned()),
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
}
