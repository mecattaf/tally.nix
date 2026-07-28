use super::*;

pub const REMOTE_EXECUTOR_PROTOCOL_VERSION: u32 = 4;
pub(super) const MAX_REMOTE_REQUEST_BYTES: u64 = 20 * 1024 * 1024;
pub(super) const MAX_REMOTE_REPLY_BYTES: usize = 48 * 1024 * 1024;
pub(super) const MAX_REMOTE_STDERR_BYTES: usize = 64 * 1024;
pub(super) const MAX_REMOTE_CAPTURE_BYTES: u64 = 16 * 1024 * 1024;

/// Wire protocol used only by the fixed `__remote-executor` helper command.
/// Job argv is nested in the JSON request and is never interpolated into the
/// OpenSSH remote command.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "kebab-case")]
pub enum RemoteExecutorRequest {
    Ensure {
        state_dir: PathBuf,
        request: ExecutionRequest,
        evidence: Vec<String>,
    },
    Adopt {
        state_dir: PathBuf,
        request: ExecutionRequest,
        expected_invocation_id: String,
        evidence: Vec<String>,
    },
    Probe {
        state_dir: PathBuf,
        identity: ExecutionIdentity,
    },
    Reclaim {
        state_dir: PathBuf,
        identity: ExecutionIdentity,
        #[serde(default)]
        expected_invocation_id: Option<String>,
        attempt: u32,
        lease_epoch: u64,
    },
}

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RemoteCapture {
    pub attempt: u32,
    pub lease_epoch: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_base64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_base64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RemoteCompletion {
    pub unit: String,
    pub record: UnitExitRecord,
    pub termination: ExecutionTermination,
    pub capture: RemoteCapture,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_gate: Option<GateResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_completion: Option<SemanticCompletion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorship: Option<Authorship>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorship_sessions: Option<Vec<AuthorshipSession>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
}

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
pub enum RemoteExecutorResult {
    Fact(LocalUnitFact),
    Completion(Box<RemoteCompletion>),
    Reclaimed(RemoteCapture),
}

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum RemoteExecutorReply {
    Ok {
        protocol_version: u32,
        result: Box<RemoteExecutorResult>,
    },
    Error {
        protocol_version: u32,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{detail}")]
pub struct RemoteTransportError {
    pub detail: String,
}

pub trait RemoteTransport: Send + Sync {
    fn call<'a>(
        &'a self,
        config: &'a SshExecutorConfig,
        request: RemoteExecutorRequest,
    ) -> Pin<Box<dyn Future<Output = Result<RemoteExecutorReply, RemoteTransportError>> + Send + 'a>>;
}

#[derive(Debug, Clone, Default)]
pub struct SshRemoteTransport;

pub fn build_ssh_argv(config: &SshExecutorConfig) -> Vec<OsString> {
    let option = |name: &str, value: String| -> OsString { format!("{name}={value}").into() };
    vec![
        "-T".into(),
        "-F".into(),
        "/dev/null".into(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "PasswordAuthentication=no".into(),
        "-o".into(),
        "KbdInteractiveAuthentication=no".into(),
        "-o".into(),
        "PubkeyAuthentication=yes".into(),
        "-o".into(),
        "IdentitiesOnly=yes".into(),
        "-o".into(),
        "IdentityAgent=none".into(),
        "-o".into(),
        "StrictHostKeyChecking=yes".into(),
        "-o".into(),
        option(
            "UserKnownHostsFile",
            config.known_hosts_file.to_string_lossy().into_owned(),
        ),
        "-o".into(),
        "GlobalKnownHostsFile=/dev/null".into(),
        "-o".into(),
        option("ConnectTimeout", config.connect_timeout_sec.to_string()),
        "-o".into(),
        "ConnectionAttempts=1".into(),
        "-o".into(),
        option(
            "ServerAliveInterval",
            config.server_alive_interval_sec.to_string(),
        ),
        "-o".into(),
        option(
            "ServerAliveCountMax",
            config.server_alive_count_max.to_string(),
        ),
        "-o".into(),
        "ClearAllForwardings=yes".into(),
        "-o".into(),
        "ForwardAgent=no".into(),
        "-o".into(),
        "ForwardX11=no".into(),
        "-o".into(),
        "PermitLocalCommand=no".into(),
        "-o".into(),
        "ProxyCommand=none".into(),
        "-o".into(),
        "CanonicalizeHostname=no".into(),
        "-o".into(),
        "LogLevel=ERROR".into(),
        "-i".into(),
        config.identity_file.as_os_str().to_owned(),
        "-p".into(),
        config.port.to_string().into(),
        "--".into(),
        format!("{}@{}", config.user, config.host).into(),
        config.program.as_os_str().to_owned(),
        "__remote-executor".into(),
    ]
}

pub(super) async fn read_async_bounded<R>(
    mut reader: R,
    limit: usize,
) -> Result<(Vec<u8>, bool), std::io::Error>
where
    R: AsyncRead + Unpin,
{
    let mut retained = Vec::new();
    let mut overflow = false;
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(retained.len());
        if read <= remaining {
            retained.extend_from_slice(&buffer[..read]);
        } else {
            retained.extend_from_slice(&buffer[..remaining]);
            overflow = true;
        }
    }
    Ok((retained, overflow))
}

impl RemoteTransport for SshRemoteTransport {
    fn call<'a>(
        &'a self,
        config: &'a SshExecutorConfig,
        request: RemoteExecutorRequest,
    ) -> Pin<Box<dyn Future<Output = Result<RemoteExecutorReply, RemoteTransportError>> + Send + 'a>>
    {
        Box::pin(async move {
            let mut encoded =
                serde_json::to_vec(&request).map_err(|error| RemoteTransportError {
                    detail: format!("cannot encode request: {error}"),
                })?;
            encoded.push(b'\n');
            if encoded.len() as u64 > MAX_REMOTE_REQUEST_BYTES {
                return Err(RemoteTransportError {
                    detail: format!(
                        "request exceeds the {MAX_REMOTE_REQUEST_BYTES}-byte protocol limit"
                    ),
                });
            }

            let mut command = Command::new(&config.ssh_program);
            command
                .kill_on_drop(true)
                .env_clear()
                .env("LC_ALL", "C")
                .args(build_ssh_argv(config))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = command.spawn().map_err(|error| RemoteTransportError {
                detail: format!("cannot spawn {}: {error}", config.ssh_program.display()),
            })?;
            let mut stdin = child.stdin.take().ok_or_else(|| RemoteTransportError {
                detail: "OpenSSH stdin pipe is unavailable".to_owned(),
            })?;
            let stdout = child.stdout.take().ok_or_else(|| RemoteTransportError {
                detail: "OpenSSH stdout pipe is unavailable".to_owned(),
            })?;
            let stderr = child.stderr.take().ok_or_else(|| RemoteTransportError {
                detail: "OpenSSH stderr pipe is unavailable".to_owned(),
            })?;
            let write = async move {
                stdin.write_all(&encoded).await?;
                stdin.shutdown().await
            };
            let wait = child.wait();
            let (write_result, status_result, stdout_result, stderr_result) = tokio::join!(
                write,
                wait,
                read_async_bounded(stdout, MAX_REMOTE_REPLY_BYTES),
                read_async_bounded(stderr, MAX_REMOTE_STDERR_BYTES),
            );
            let status = status_result.map_err(|error| RemoteTransportError {
                detail: format!("cannot wait for OpenSSH: {error}"),
            })?;
            let (stdout, stdout_overflow) =
                stdout_result.map_err(|error| RemoteTransportError {
                    detail: format!("cannot read OpenSSH stdout: {error}"),
                })?;
            let (stderr, stderr_overflow) =
                stderr_result.map_err(|error| RemoteTransportError {
                    detail: format!("cannot read OpenSSH stderr: {error}"),
                })?;
            if let Err(error) = write_result {
                return Err(RemoteTransportError {
                    detail: format!("cannot send remote request: {error}"),
                });
            }
            if stdout_overflow {
                return Err(RemoteTransportError {
                    detail: format!("remote reply exceeds {MAX_REMOTE_REPLY_BYTES} bytes"),
                });
            }
            let stderr_text = String::from_utf8_lossy(&stderr).trim().to_owned();
            if !status.success() {
                return Err(RemoteTransportError {
                    detail: format!(
                        "OpenSSH exited with status {:?}: {}{}",
                        status.code(),
                        stderr_text,
                        if stderr_overflow {
                            " (stderr truncated)"
                        } else {
                            ""
                        }
                    ),
                });
            }
            serde_json::from_slice(&stdout).map_err(|error| RemoteTransportError {
                detail: format!(
                    "remote helper returned invalid JSON: {error}; stderr={stderr_text:?}"
                ),
            })
        })
    }
}

impl Executor {
    pub(super) fn remote_config(&self, name: &str) -> Result<SshExecutorConfig, ExecutorError> {
        self.remote_executors
            .get(name)
            .map(ExecutionTargetConfig::ssh)
            .cloned()
            .ok_or_else(|| ExecutorError::UnknownRemoteExecutor(name.to_owned()))
    }

    pub(super) async fn call_remote(
        &self,
        name: &str,
        request: RemoteExecutorRequest,
    ) -> Result<RemoteExecutorResult, ExecutorError> {
        let config = self.remote_config(name)?;
        let encoded_len = serde_json::to_vec(&request)
            .map_err(|error| ExecutorError::RemoteProtocol {
                executor: name.to_owned(),
                detail: format!("cannot encode request: {error}"),
            })?
            .len()
            .saturating_add(1);
        if encoded_len as u64 > MAX_REMOTE_REQUEST_BYTES {
            return Err(ExecutorError::RemoteProtocol {
                executor: name.to_owned(),
                detail: format!(
                    "request exceeds the {MAX_REMOTE_REQUEST_BYTES}-byte protocol limit"
                ),
            });
        }
        let mut reported_loss = false;
        loop {
            match self.remote_transport.call(&config, request.clone()).await {
                Ok(RemoteExecutorReply::Ok {
                    protocol_version,
                    result,
                }) => {
                    if protocol_version != REMOTE_EXECUTOR_PROTOCOL_VERSION {
                        return Err(ExecutorError::RemoteProtocol {
                            executor: name.to_owned(),
                            detail: format!(
                                "helper protocol version {protocol_version}, expected {REMOTE_EXECUTOR_PROTOCOL_VERSION}"
                            ),
                        });
                    }
                    if reported_loss {
                        eprintln!("tally: remote executor {name:?} is reachable again");
                    }
                    return Ok(*result);
                }
                Ok(RemoteExecutorReply::Error {
                    protocol_version,
                    message,
                }) => {
                    if protocol_version != REMOTE_EXECUTOR_PROTOCOL_VERSION {
                        return Err(ExecutorError::RemoteProtocol {
                            executor: name.to_owned(),
                            detail: format!(
                                "helper error protocol version {protocol_version}, expected {REMOTE_EXECUTOR_PROTOCOL_VERSION}"
                            ),
                        });
                    }
                    if message.starts_with("git-ai-") {
                        return Err(ExecutorError::GitAiRequired(message));
                    }
                    return Err(ExecutorError::RemoteExecution {
                        executor: name.to_owned(),
                        detail: message,
                    });
                }
                Err(error) => {
                    if !reported_loss {
                        eprintln!(
                            "tally: remote executor {name:?} transport is unavailable; retaining leases and retrying: {error}"
                        );
                        reported_loss = true;
                    }
                    tokio::time::sleep(Duration::from_millis(config.retry_interval_ms)).await;
                }
            }
        }
    }

    pub(super) fn materialize_remote_capture(
        &self,
        identity: &ExecutionIdentity,
        expected_attempt: u32,
        expected_lease_epoch: u64,
        capture: &RemoteCapture,
    ) -> Result<bool, ExecutorError> {
        if capture.attempt != expected_attempt || capture.lease_epoch != expected_lease_epoch {
            return Err(ExecutorError::InvalidRequest(format!(
                "remote capture generation attempt={} leaseEpoch={} does not match expected attempt={expected_attempt} leaseEpoch={expected_lease_epoch}",
                capture.attempt, capture.lease_epoch
            )));
        }
        if let Some(error) = &capture.error {
            eprintln!(
                "tally: remote capture for {} is unavailable: {error}",
                identity.unit_uuid()
            );
            return Ok(false);
        }
        let (Some(stdout), Some(stderr)) = (
            capture.stdout_base64.as_deref(),
            capture.stderr_base64.as_deref(),
        ) else {
            return Err(ExecutorError::InvalidRequest(
                "remote capture omitted data without an error".to_owned(),
            ));
        };
        let stdout = decode_base64(stdout)?;
        let stderr = decode_base64(stderr)?;
        if !self.capture_generation_matches(identity, expected_attempt, expected_lease_epoch)? {
            self.archive_current_capture(identity)?;
        }
        let paths = self.paths(identity);
        replace_private_file(&paths.stdout, &stdout)?;
        replace_private_file(&paths.stderr, &stderr)?;
        write_capture_generation(
            &paths.capture_generation,
            CaptureGeneration {
                attempt: expected_attempt,
                lease_epoch: expected_lease_epoch,
            },
        )?;
        Ok(true)
    }

    pub(super) fn materialize_remote_completion(
        &self,
        executor_name: &str,
        identity: &ExecutionIdentity,
        expected_invocation_id: Option<&str>,
        expected_attempt: u32,
        expected_lease_epoch: u64,
        completion: RemoteCompletion,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        let expected_unit = self.unit_name(identity);
        if completion.unit != expected_unit {
            return Err(ExecutorError::RemoteProtocol {
                executor: executor_name.to_owned(),
                detail: format!(
                    "helper returned unit {:?}, expected {expected_unit:?}",
                    completion.unit
                ),
            });
        }
        completion
            .record
            .validate(&expected_unit)
            .map_err(|error| ExecutorError::RemoteProtocol {
                executor: executor_name.to_owned(),
                detail: error.to_string(),
            })?;
        if let Some(expected) = expected_invocation_id {
            if completion.record.invocation_id != expected {
                return Err(ExecutorError::AdoptedInvocationMismatch {
                    unit: expected_unit,
                    expected: expected.to_owned(),
                    observed: Some(completion.record.invocation_id),
                });
            }
        }
        if completion.record.attempt != expected_attempt
            || completion.record.lease_epoch != expected_lease_epoch
        {
            return Err(ExecutorError::AdoptedGenerationMismatch {
                unit: expected_unit,
                expected_attempt,
                expected_lease_epoch,
                observed_attempt: completion.record.attempt,
                observed_lease_epoch: completion.record.lease_epoch,
            });
        }
        let classified = classify_termination(&completion.record).map_err(|error| {
            ExecutorError::RemoteProtocol {
                executor: executor_name.to_owned(),
                detail: error.to_string(),
            }
        })?;
        if completion.termination != classified {
            return Err(ExecutorError::RemoteProtocol {
                executor: executor_name.to_owned(),
                detail: format!(
                    "helper termination {:?} does not match durable exit record classification {:?}",
                    completion.termination, classified
                ),
            });
        }
        if matches!(completion.termination, ExecutionTermination::Exited(_))
            != completion.evidence_gate.is_some()
        {
            return Err(ExecutorError::RemoteProtocol {
                executor: executor_name.to_owned(),
                detail: "helper evidence result does not match the terminal state".to_owned(),
            });
        }
        let captures_available = match self.materialize_remote_capture(
            identity,
            expected_attempt,
            expected_lease_epoch,
            &completion.capture,
        ) {
            Ok(available) => available,
            Err(ExecutorError::InvalidRequest(detail)) => {
                return Err(ExecutorError::RemoteProtocol {
                    executor: executor_name.to_owned(),
                    detail,
                });
            }
            Err(error) => {
                eprintln!(
                    "tally: remote execution completed, but its local capture cache could not be written: {error}"
                );
                false
            }
        };
        let paths = self.paths(identity);
        if let Err(error) = write_exit_record(&paths.exit_record, &completion.record) {
            eprintln!(
                "tally: remote execution completed, but its local exit cache could not be written: {error}"
            );
        }
        Ok(ExecutionOutcome {
            unit: completion.unit,
            backend: ExecutionBackend::Remote,
            paths,
            record: completion.record,
            termination: completion.termination,
            evidence_gate: completion.evidence_gate,
            semantic_completion: completion.semantic_completion,
            result_revision: completion.result_revision,
            authorship: completion.authorship,
            authorship_sessions: completion.authorship_sessions,
            host_id: completion.host_id,
            captures_available,
        })
    }

    pub async fn inspect_identity_on(
        &self,
        executor: Option<&str>,
        identity: &ExecutionIdentity,
    ) -> Result<LocalUnitFact, ExecutorError> {
        let Some(name) = executor else {
            return self.inspect_identity_async(identity).await;
        };
        let config = self.remote_config(name)?;
        let result = self
            .call_remote(
                name,
                RemoteExecutorRequest::Probe {
                    state_dir: config.state_dir,
                    identity: identity.clone(),
                },
            )
            .await?;
        let RemoteExecutorResult::Fact(fact) = result else {
            return Err(ExecutorError::RemoteProtocol {
                executor: name.to_owned(),
                detail: "probe returned a non-fact response".to_owned(),
            });
        };
        let unit = self.unit_name(identity);
        validate_local_unit_fact_shape(&unit, &fact)?;
        Ok(fact)
    }

    pub async fn execute_on(
        &self,
        executor: Option<&str>,
        mut request: ExecutionRequest,
        evidence: Vec<String>,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        let Some(name) = executor else {
            return self.execute(request).await;
        };
        self.validate_request(&request)?;
        self.embed_brief_for_remote(&mut request)?;
        self.validate_request(&request)?;
        parse_evidence_specs(&evidence)
            .map_err(|error| ExecutorError::InvalidRequest(error.to_string()))?;
        let config = self.remote_config(name)?;
        let identity = request.identity.clone();
        let attempt = request.attempt;
        let lease_epoch = request.lease_epoch;
        let result = self
            .call_remote(
                name,
                RemoteExecutorRequest::Ensure {
                    state_dir: config.state_dir,
                    request,
                    evidence,
                },
            )
            .await?;
        let RemoteExecutorResult::Completion(completion) = result else {
            return Err(ExecutorError::RemoteProtocol {
                executor: name.to_owned(),
                detail: "ensure returned a non-completion response".to_owned(),
            });
        };
        self.materialize_remote_completion(name, &identity, None, attempt, lease_epoch, *completion)
    }

    pub async fn adopt_on(
        &self,
        executor: Option<&str>,
        request: ExecutionRequest,
        expected_invocation_id: &str,
        evidence: Vec<String>,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        let Some(name) = executor else {
            return self.adopt(request, expected_invocation_id).await;
        };
        self.validate_request(&request)?;
        parse_evidence_specs(&evidence)
            .map_err(|error| ExecutorError::InvalidRequest(error.to_string()))?;
        let config = self.remote_config(name)?;
        let identity = request.identity.clone();
        let attempt = request.attempt;
        let lease_epoch = request.lease_epoch;
        let result = self
            .call_remote(
                name,
                RemoteExecutorRequest::Adopt {
                    state_dir: config.state_dir,
                    request,
                    expected_invocation_id: expected_invocation_id.to_owned(),
                    evidence,
                },
            )
            .await?;
        let RemoteExecutorResult::Completion(completion) = result else {
            return Err(ExecutorError::RemoteProtocol {
                executor: name.to_owned(),
                detail: "adopt returned a non-completion response".to_owned(),
            });
        };
        self.materialize_remote_completion(
            name,
            &identity,
            Some(expected_invocation_id),
            attempt,
            lease_epoch,
            *completion,
        )
    }

    pub async fn reclaim_identity_exact_on(
        &self,
        executor: Option<&str>,
        identity: &ExecutionIdentity,
        expected_invocation_id: Option<&str>,
        attempt: u32,
        lease_epoch: u64,
    ) -> Result<(), ExecutorError> {
        let Some(name) = executor else {
            return self
                .reclaim_identity_exact(identity, expected_invocation_id)
                .await;
        };
        let config = self.remote_config(name)?;
        let result = self
            .call_remote(
                name,
                RemoteExecutorRequest::Reclaim {
                    state_dir: config.state_dir,
                    identity: identity.clone(),
                    expected_invocation_id: expected_invocation_id.map(ToOwned::to_owned),
                    attempt,
                    lease_epoch,
                },
            )
            .await?;
        let RemoteExecutorResult::Reclaimed(capture) = result else {
            return Err(ExecutorError::RemoteProtocol {
                executor: name.to_owned(),
                detail: "reclaim returned a non-reclaimed response".to_owned(),
            });
        };
        self.materialize_remote_capture(identity, attempt, lease_epoch, &capture)?;
        Ok(())
    }
}

pub(super) fn decode_base64(value: &str) -> Result<Vec<u8>, ExecutorError> {
    fn digit(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    if !value.len().is_multiple_of(4)
        || value.len() > (MAX_REMOTE_CAPTURE_BYTES as usize).div_ceil(3) * 4
    {
        return Err(ExecutorError::InvalidRequest(
            "remote capture has an invalid base64 length".to_owned(),
        ));
    }
    let mut output = Vec::with_capacity(value.len() / 4 * 3);
    for (index, chunk) in value.as_bytes().chunks_exact(4).enumerate() {
        let last = index + 1 == value.len() / 4;
        let a = digit(chunk[0]).ok_or_else(|| {
            ExecutorError::InvalidRequest("remote capture contains invalid base64".to_owned())
        })?;
        let b = digit(chunk[1]).ok_or_else(|| {
            ExecutorError::InvalidRequest("remote capture contains invalid base64".to_owned())
        })?;
        let c = if chunk[2] == b'=' {
            if !last || chunk[3] != b'=' || b & 0x0f != 0 {
                return Err(ExecutorError::InvalidRequest(
                    "remote capture has non-canonical base64 padding".to_owned(),
                ));
            }
            None
        } else {
            Some(digit(chunk[2]).ok_or_else(|| {
                ExecutorError::InvalidRequest("remote capture contains invalid base64".to_owned())
            })?)
        };
        let d = if chunk[3] == b'=' {
            if !last || c.is_some_and(|value| value & 0x03 != 0) {
                return Err(ExecutorError::InvalidRequest(
                    "remote capture has non-canonical base64 padding".to_owned(),
                ));
            }
            None
        } else {
            Some(digit(chunk[3]).ok_or_else(|| {
                ExecutorError::InvalidRequest("remote capture contains invalid base64".to_owned())
            })?)
        };
        output.push((a << 2) | (b >> 4));
        if let Some(c) = c {
            output.push((b << 4) | (c >> 2));
            if let Some(d) = d {
                output.push((c << 6) | d);
            }
        }
    }
    if output.len() as u64 > MAX_REMOTE_CAPTURE_BYTES {
        return Err(ExecutorError::InvalidRequest(
            "remote capture exceeds its decoded byte limit".to_owned(),
        ));
    }
    Ok(output)
}

pub(super) fn read_remote_capture(path: &Path) -> Result<Vec<u8>, ExecutorError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|source| io_error(path, source))?;
    let metadata = file.metadata().map_err(|source| io_error(path, source))?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_REMOTE_CAPTURE_BYTES {
        return Err(ExecutorError::InvalidRequest(format!(
            "capture {} is not a bounded regular file",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    file.take(MAX_REMOTE_CAPTURE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    if bytes.len() as u64 > MAX_REMOTE_CAPTURE_BYTES {
        return Err(ExecutorError::InvalidRequest(format!(
            "capture {} exceeds {MAX_REMOTE_CAPTURE_BYTES} bytes",
            path.display()
        )));
    }
    Ok(bytes)
}

pub(super) fn collect_remote_capture(
    paths: &ExecutionPaths,
    attempt: u32,
    lease_epoch: u64,
) -> RemoteCapture {
    match (
        read_remote_capture(&paths.stdout),
        read_remote_capture(&paths.stderr),
    ) {
        (Ok(stdout), Ok(stderr)) => RemoteCapture {
            attempt,
            lease_epoch,
            stdout_base64: Some(encode_base64(&stdout)),
            stderr_base64: Some(encode_base64(&stderr)),
            error: None,
        },
        (stdout, stderr) => RemoteCapture {
            attempt,
            lease_epoch,
            stdout_base64: None,
            stderr_base64: None,
            error: Some(format!(
                "stdout: {}; stderr: {}",
                stdout
                    .err()
                    .map_or_else(|| "ok".to_owned(), |error| error.to_string()),
                stderr
                    .err()
                    .map_or_else(|| "ok".to_owned(), |error| error.to_string())
            )),
        },
    }
}

pub(super) fn remote_completion(
    outcome: ExecutionOutcome,
    evidence: &[String],
) -> Result<RemoteCompletion, ExecutorError> {
    let gate = match &outcome.termination {
        ExecutionTermination::Exited(exit_code) => {
            let spec = parse_evidence_specs(evidence)
                .map_err(|error| ExecutorError::InvalidRequest(error.to_string()))?;
            Some(run_evidence_gate(RunOutcome {
                exit_code: *exit_code,
                wall_clock_seconds: 0.0,
                evidence: &spec,
            }))
        }
        _ => None,
    };
    let capture = collect_remote_capture(
        &outcome.paths,
        outcome.record.attempt,
        outcome.record.lease_epoch,
    );
    Ok(RemoteCompletion {
        unit: outcome.unit,
        record: outcome.record,
        termination: outcome.termination,
        capture,
        evidence_gate: gate,
        semantic_completion: outcome.semantic_completion,
        result_revision: outcome.result_revision,
        authorship: outcome.authorship,
        authorship_sessions: outcome.authorship_sessions,
        host_id: outcome.host_id,
    })
}

pub(super) fn execution_fact(termination: &ExecutionTermination) -> ExecutionFact {
    match termination {
        ExecutionTermination::Exited(exit_code) => ExecutionFact::exited(*exit_code),
        ExecutionTermination::RuntimeExceeded => {
            ExecutionFact::failed("process exceeded RuntimeMaxSec")
        }
        ExecutionTermination::Signaled { code, status } => {
            ExecutionFact::failed(format!("process ended by {code} {status}"))
        }
        ExecutionTermination::ServiceFailed { service_result, .. } => {
            ExecutionFact::failed(format!("systemd service failed with {service_result}"))
        }
    }
}

pub(super) fn pin_remote_reclaim(
    fact: &LocalUnitFact,
    expected_invocation_id: Option<&str>,
    expected_attempt: u32,
    expected_lease_epoch: u64,
) -> Result<Option<String>, ExecutorError> {
    if matches!(fact.state, LocalUnitState::Running | LocalUnitState::Exited) {
        let observed_attempt = fact.attempt.expect("validated present fact has an attempt");
        let observed_lease_epoch = fact
            .lease_epoch
            .expect("validated present fact has a lease epoch");
        if observed_attempt != expected_attempt || observed_lease_epoch != expected_lease_epoch {
            return Err(ExecutorError::AdoptedGenerationMismatch {
                unit: fact.unit.clone(),
                expected_attempt,
                expected_lease_epoch,
                observed_attempt,
                observed_lease_epoch,
            });
        }
    }
    if let Some(expected) = expected_invocation_id {
        if fact.state != LocalUnitState::Absent && fact.invocation_id.as_deref() != Some(expected) {
            return Err(ExecutorError::AdoptedInvocationMismatch {
                unit: fact.unit.clone(),
                expected: expected.to_owned(),
                observed: fact.invocation_id.clone(),
            });
        }
        return Ok(Some(expected.to_owned()));
    }
    Ok(fact.invocation_id.clone())
}

pub(super) async fn ensure_local_execution(
    executor: &Executor,
    request: ExecutionRequest,
) -> Result<ExecutionOutcome, ExecutorError> {
    loop {
        match executor.execute(request.clone()).await {
            Ok(outcome) => return Ok(outcome),
            Err(
                error @ (ExecutorError::AlreadyRunning(_) | ExecutorError::ExistingUnit { .. }),
            ) => {
                let fact = executor.inspect_identity_async(&request.identity).await?;
                match fact.state {
                    LocalUnitState::Absent => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    LocalUnitState::Running => {
                        if fact.attempt != Some(request.attempt)
                            || fact.lease_epoch != Some(request.lease_epoch)
                        {
                            return Err(ExecutorError::AdoptedGenerationMismatch {
                                unit: fact.unit,
                                expected_attempt: request.attempt,
                                expected_lease_epoch: request.lease_epoch,
                                observed_attempt: fact.attempt.unwrap_or_default(),
                                observed_lease_epoch: fact.lease_epoch.unwrap_or_default(),
                            });
                        }
                        let invocation =
                            fact.invocation_id.ok_or_else(|| ExecutorError::UnitProbe {
                                unit: fact.unit.clone(),
                                detail: "running remote unit has no invocation identity".to_owned(),
                            })?;
                        return executor.adopt(request, &invocation).await;
                    }
                    LocalUnitState::InactiveWithoutRecord => {
                        let invocation =
                            fact.invocation_id.ok_or_else(|| ExecutorError::UnitProbe {
                                unit: fact.unit.clone(),
                                detail: "inactive remote unit has no invocation identity"
                                    .to_owned(),
                            })?;
                        return executor.adopt(request, &invocation).await;
                    }
                    LocalUnitState::Exited
                        if fact.exit_record.as_ref().is_some_and(|record| {
                            record.attempt == request.attempt
                                && record.lease_epoch == request.lease_epoch
                        }) =>
                    {
                        // `execute` consumes a matching durable exit without
                        // launching, so retrying here is idempotent.
                    }
                    LocalUnitState::Exited => return Err(error),
                }
            }
            Err(error) => return Err(error),
        }
    }
}

pub(super) async fn handle_remote_executor_request(
    request: RemoteExecutorRequest,
) -> Result<RemoteExecutorResult, ExecutorError> {
    let state_dir = match &request {
        RemoteExecutorRequest::Ensure { state_dir, .. }
        | RemoteExecutorRequest::Adopt { state_dir, .. }
        | RemoteExecutorRequest::Probe { state_dir, .. }
        | RemoteExecutorRequest::Reclaim { state_dir, .. } => state_dir.clone(),
    };
    if !state_dir.is_absolute() {
        return Err(ExecutorError::InvalidRequest(
            "remote stateDir must be absolute".to_owned(),
        ));
    }
    validate_systemd_path(&state_dir, "remote stateDir")?;
    let recorder = std::env::current_exe().map_err(|source| ExecutorError::Io {
        path: PathBuf::from("/proc/self/exe"),
        source,
    })?;
    let executor = Executor::new(state_dir, recorder).require_systemd();
    match request {
        RemoteExecutorRequest::Ensure {
            request, evidence, ..
        } => Ok(RemoteExecutorResult::Completion(Box::new(
            remote_completion(ensure_local_execution(&executor, request).await?, &evidence)?,
        ))),
        RemoteExecutorRequest::Adopt {
            request,
            expected_invocation_id,
            evidence,
            ..
        } => Ok(RemoteExecutorResult::Completion(Box::new(
            remote_completion(
                executor.adopt(request, &expected_invocation_id).await?,
                &evidence,
            )?,
        ))),
        RemoteExecutorRequest::Probe { identity, .. } => Ok(RemoteExecutorResult::Fact(
            executor.inspect_identity_async(&identity).await?,
        )),
        RemoteExecutorRequest::Reclaim {
            identity,
            expected_invocation_id,
            attempt,
            lease_epoch,
            ..
        } => {
            if attempt == 0 || lease_epoch == 0 {
                return Err(ExecutorError::InvalidRequest(
                    "remote reclaim generation must be positive".to_owned(),
                ));
            }
            let fact = executor.inspect_identity_async(&identity).await?;
            let pinned_invocation = pin_remote_reclaim(
                &fact,
                expected_invocation_id.as_deref(),
                attempt,
                lease_epoch,
            )?;
            executor
                .reclaim_identity_exact(&identity, pinned_invocation.as_deref())
                .await?;
            Ok(RemoteExecutorResult::Reclaimed(collect_remote_capture(
                &executor.paths(&identity),
                attempt,
                lease_epoch,
            )))
        }
    }
}

/// Serve exactly one bounded remote-executor request over stdin/stdout.
/// Errors are returned as structured protocol replies so the coordinator can
/// fail closed without guessing whether stderr came from OpenSSH or tally.
pub async fn serve_remote_executor_stdio() -> Result<(), ExecutorError> {
    let request = (|| -> Result<RemoteExecutorRequest, ExecutorError> {
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(MAX_REMOTE_REQUEST_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|source| ExecutorError::Io {
                path: PathBuf::from("<stdin>"),
                source,
            })?;
        if bytes.len() as u64 > MAX_REMOTE_REQUEST_BYTES {
            return Err(ExecutorError::InvalidRequest(format!(
                "remote request exceeds {MAX_REMOTE_REQUEST_BYTES} bytes"
            )));
        }
        serde_json::from_slice(&bytes).map_err(ExecutorError::from)
    })();
    let reply = match request {
        Ok(request) => match handle_remote_executor_request(request).await {
            Ok(result) => RemoteExecutorReply::Ok {
                protocol_version: REMOTE_EXECUTOR_PROTOCOL_VERSION,
                result: Box::new(result),
            },
            Err(error) => RemoteExecutorReply::Error {
                protocol_version: REMOTE_EXECUTOR_PROTOCOL_VERSION,
                message: error.to_string(),
            },
        },
        Err(error) => RemoteExecutorReply::Error {
            protocol_version: REMOTE_EXECUTOR_PROTOCOL_VERSION,
            message: error.to_string(),
        },
    };
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &reply)?;
    stdout
        .write_all(b"\n")
        .and_then(|()| stdout.flush())
        .map_err(|source| ExecutorError::Io {
            path: PathBuf::from("<stdout>"),
            source,
        })
}
