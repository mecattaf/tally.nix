use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json_path::JsonPath;
use thiserror::Error;

use crate::executor::ExecutionPaths;

const MAX_CAPTURE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScrapeStream {
    #[default]
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScrapeMode {
    #[default]
    Regex,
    JsonPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ScrapeCapture {
    #[serde(default)]
    pub stream: ScrapeStream,
    #[serde(default)]
    pub mode: ScrapeMode,
    pub pattern: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AdapterConfig {
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default)]
    pub resume: Option<Vec<String>>,
    #[serde(default)]
    pub scrape: BTreeMap<String, ScrapeCapture>,
    #[serde(default)]
    pub yield_hook: Option<Vec<String>>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub extra_config: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AdapterInvocation {
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// Direct checkpoint probe installed by the harness integration. Tally
    /// deliberately does not run it on a timer: the harness owns its safe
    /// checkpoints, and the no-argument preset probe resolves TALLY_JOB_ID.
    pub yield_hook: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScrapeResult {
    pub captures: BTreeMap<String, Value>,
}

impl ScrapeResult {
    pub fn string(&self, name: &str) -> Result<Option<&str>, AdapterError> {
        match self.captures.get(name) {
            None => Ok(None),
            Some(Value::String(value)) => Ok(Some(value)),
            Some(_) => Err(AdapterError::ReservedCaptureType(name.to_owned())),
        }
    }

    pub fn session_ref(&self) -> Result<Option<&str>, AdapterError> {
        self.string("sessionRef")
    }

    pub fn model(&self) -> Result<Option<&str>, AdapterError> {
        self.string("model")
    }
}

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("unknown adapter {0:?}")]
    UnknownAdapter(String),
    #[error("invalid adapter {adapter:?}: {detail}")]
    InvalidConfig { adapter: String, detail: String },
    #[error("adapter {0:?} has no resume template")]
    NoResume(String),
    #[error("resume capture {capture:?} is absent for adapter {adapter:?}")]
    MissingCapture { adapter: String, capture: String },
    #[error("reserved capture {0:?} must be a JSON string")]
    ReservedCaptureType(String),
    #[error("capture path {path} cannot be read: {source}")]
    CaptureRead { path: String, source: io::Error },
    #[error("capture path {0} is not a regular file")]
    CaptureNotRegular(String),
    #[error("capture path {path} exceeds the {limit}-byte scrape bound")]
    CaptureTooLarge { path: String, limit: u64 },
    #[error("capture path {0} is not UTF-8")]
    CaptureNotUtf8(String),
    #[error("capture {capture:?} could not be scraped: {detail}")]
    Scrape { capture: String, detail: String },
}

pub struct AdapterEngine<'a> {
    adapters: &'a BTreeMap<String, AdapterConfig>,
}

impl<'a> AdapterEngine<'a> {
    pub const fn new(adapters: &'a BTreeMap<String, AdapterConfig>) -> Self {
        Self { adapters }
    }

    pub fn validate_all(&self) -> Result<(), AdapterError> {
        for (name, adapter) in self.adapters {
            validate_adapter_name(name)?;
            validate_adapter(name, adapter)?;
        }
        Ok(())
    }

    pub fn adapter(&self, name: &str) -> Result<&AdapterConfig, AdapterError> {
        self.adapters
            .get(name)
            .ok_or_else(|| AdapterError::UnknownAdapter(name.to_owned()))
    }

    pub fn launch(
        &self,
        name: &str,
        workload_argv: &[String],
    ) -> Result<AdapterInvocation, AdapterError> {
        let adapter = self.adapter(name)?;
        invocation(name, adapter, compose_argv(&adapter.argv, workload_argv))
    }

    pub fn resume(
        &self,
        name: &str,
        workload_argv: &[String],
        captures: &ScrapeResult,
    ) -> Result<AdapterInvocation, AdapterError> {
        let adapter = self.adapter(name)?;
        let template = adapter
            .resume
            .as_deref()
            .ok_or_else(|| AdapterError::NoResume(name.to_owned()))?;
        let rendered = template
            .iter()
            .map(|argument| render_argument(name, argument, &captures.captures))
            .collect::<Result<Vec<_>, _>>()?;
        invocation(name, adapter, compose_argv(&rendered, workload_argv))
    }

    pub fn scrape_text(
        &self,
        name: &str,
        stdout: &str,
        stderr: &str,
    ) -> Result<ScrapeResult, AdapterError> {
        let adapter = self.adapter(name)?;
        let mut captures = BTreeMap::new();
        for (capture_name, capture) in &adapter.scrape {
            let input = match capture.stream {
                ScrapeStream::Stdout => stdout,
                ScrapeStream::Stderr => stderr,
            };
            let value = match capture.mode {
                ScrapeMode::Regex => scrape_regex(&capture.pattern, input),
                ScrapeMode::JsonPath => scrape_json_path(&capture.pattern, input),
            }
            .map_err(|detail| AdapterError::Scrape {
                capture: capture_name.clone(),
                detail,
            })?;
            if let Some(value) = value {
                captures.insert(capture_name.clone(), value);
            }
        }
        let result = ScrapeResult { captures };
        result.session_ref()?;
        result.model()?;
        Ok(result)
    }

    pub fn scrape_paths(
        &self,
        name: &str,
        paths: &ExecutionPaths,
    ) -> Result<ScrapeResult, AdapterError> {
        let adapter = self.adapter(name)?;
        let needs_stdout = adapter
            .scrape
            .values()
            .any(|capture| capture.stream == ScrapeStream::Stdout);
        let needs_stderr = adapter
            .scrape
            .values()
            .any(|capture| capture.stream == ScrapeStream::Stderr);
        let stdout = if needs_stdout {
            read_capture(&paths.stdout)?
        } else {
            String::new()
        };
        let stderr = if needs_stderr {
            read_capture(&paths.stderr)?
        } else {
            String::new()
        };
        self.scrape_text(name, &stdout, &stderr)
    }
}

fn invocation(
    name: &str,
    adapter: &AdapterConfig,
    argv: Vec<String>,
) -> Result<AdapterInvocation, AdapterError> {
    if argv.is_empty() || argv[0].is_empty() {
        return Err(AdapterError::InvalidConfig {
            adapter: name.to_owned(),
            detail: "rendered argv must contain a non-empty executable".to_owned(),
        });
    }
    if argv.iter().any(|argument| argument.contains('\0')) {
        return Err(AdapterError::InvalidConfig {
            adapter: name.to_owned(),
            detail: "rendered argv must not contain NUL bytes".to_owned(),
        });
    }
    Ok(AdapterInvocation {
        argv,
        env: adapter.env.clone(),
        yield_hook: adapter.yield_hook.clone(),
    })
}

fn compose_argv(prefix: &[String], workload: &[String]) -> Vec<String> {
    prefix.iter().chain(workload).cloned().collect()
}

fn validate_adapter_name(name: &str) -> Result<(), AdapterError> {
    if name.trim().is_empty() || name.chars().any(char::is_control) {
        return Err(AdapterError::InvalidConfig {
            adapter: name.to_owned(),
            detail: "name must be non-empty and contain no control characters".to_owned(),
        });
    }
    Ok(())
}

fn validate_adapter(name: &str, adapter: &AdapterConfig) -> Result<(), AdapterError> {
    validate_argv(name, "argv", &adapter.argv, true)?;
    if let Some(resume) = &adapter.resume {
        validate_argv(name, "resume", resume, false)?;
    }
    if let Some(hook) = &adapter.yield_hook {
        validate_argv(name, "yieldHook", hook, false)?;
    }
    for (env_name, value) in &adapter.env {
        if !valid_environment_name(env_name)
            || env_name.starts_with("TALLY_")
            || env_name == "CREDENTIALS_DIRECTORY"
        {
            return invalid_config(
                name,
                format!("environment name {env_name:?} is invalid or reserved"),
            );
        }
        if value.contains('\0') {
            return invalid_config(
                name,
                format!("environment {env_name:?} contains a NUL byte"),
            );
        }
    }
    let mut names = BTreeSet::new();
    for (capture_name, capture) in &adapter.scrape {
        if !valid_capture_name(capture_name) {
            return invalid_config(name, format!("invalid capture name {capture_name:?}"));
        }
        names.insert(capture_name.as_str());
        if capture.pattern.is_empty() || capture.pattern.contains('\0') {
            return invalid_config(
                name,
                format!("capture {capture_name:?} has an empty or NUL pattern"),
            );
        }
        match capture.mode {
            ScrapeMode::Regex => {
                Regex::new(&capture.pattern).map_err(|error| AdapterError::InvalidConfig {
                    adapter: name.to_owned(),
                    detail: format!("capture {capture_name:?} has invalid regex: {error}"),
                })?;
            }
            ScrapeMode::JsonPath => {
                parse_json_path(&capture.pattern).map_err(|detail| {
                    AdapterError::InvalidConfig {
                        adapter: name.to_owned(),
                        detail: format!("capture {capture_name:?} has invalid JSONPath: {detail}"),
                    }
                })?;
            }
        }
    }
    if let Some(template) = &adapter.resume {
        for argument in template {
            for capture in placeholders(argument).map_err(|detail| AdapterError::InvalidConfig {
                adapter: name.to_owned(),
                detail: format!("invalid resume argument {argument:?}: {detail}"),
            })? {
                if !names.contains(capture.as_str()) {
                    return invalid_config(
                        name,
                        format!("resume references unknown capture {capture:?}"),
                    );
                }
            }
        }
    }
    Ok(())
}

fn validate_argv(
    adapter: &str,
    field: &str,
    argv: &[String],
    allow_empty: bool,
) -> Result<(), AdapterError> {
    if !allow_empty && argv.is_empty() {
        return invalid_config(adapter, format!("{field} must not be an empty argv"));
    }
    if argv.first().is_some_and(String::is_empty) {
        return invalid_config(adapter, format!("{field} executable must not be empty"));
    }
    if argv.iter().any(|value| value.contains('\0')) {
        return invalid_config(
            adapter,
            format!("{field} arguments must contain no NUL bytes"),
        );
    }
    Ok(())
}

fn invalid_config<T>(adapter: &str, detail: String) -> Result<T, AdapterError> {
    Err(AdapterError::InvalidConfig {
        adapter: adapter.to_owned(),
        detail,
    })
}

fn valid_environment_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn valid_capture_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn placeholders(argument: &str) -> Result<Vec<String>, String> {
    let mut captures = Vec::new();
    let mut rest = argument;
    while let Some(start) = rest.find("%<") {
        rest = &rest[start + 2..];
        let end = rest
            .find(">%")
            .ok_or_else(|| "unclosed '%<captureName>%' placeholder".to_owned())?;
        let name = &rest[..end];
        if !valid_capture_name(name) {
            return Err(format!("invalid capture placeholder {name:?}"));
        }
        captures.push(name.to_owned());
        rest = &rest[end + 2..];
    }
    Ok(captures)
}

fn render_argument(
    adapter: &str,
    argument: &str,
    captures: &BTreeMap<String, Value>,
) -> Result<String, AdapterError> {
    let mut output = String::new();
    let mut rest = argument;
    while let Some(start) = rest.find("%<") {
        output.push_str(&rest[..start]);
        rest = &rest[start + 2..];
        let end = rest.find(">%").ok_or_else(|| AdapterError::InvalidConfig {
            adapter: adapter.to_owned(),
            detail: "unclosed '%<captureName>%' placeholder".to_owned(),
        })?;
        let capture = &rest[..end];
        if !valid_capture_name(capture) {
            return Err(AdapterError::InvalidConfig {
                adapter: adapter.to_owned(),
                detail: format!("invalid capture placeholder {capture:?}"),
            });
        }
        let value = captures
            .get(capture)
            .ok_or_else(|| AdapterError::MissingCapture {
                adapter: adapter.to_owned(),
                capture: capture.to_owned(),
            })?;
        match value {
            Value::String(value) => output.push_str(value),
            value => output.push_str(&serde_json::to_string(value).map_err(|error| {
                AdapterError::Scrape {
                    capture: capture.to_owned(),
                    detail: error.to_string(),
                }
            })?),
        }
        rest = &rest[end + 2..];
    }
    output.push_str(rest);
    Ok(output)
}

fn scrape_regex(pattern: &str, input: &str) -> Result<Option<Value>, String> {
    let expression = Regex::new(pattern).map_err(|error| error.to_string())?;
    let mut selected = None;
    for captures in expression.captures_iter(input) {
        let value = if captures.len() > 1 {
            captures
                .get(1)
                .ok_or_else(|| "the first regex capture group did not participate".to_owned())?
        } else {
            captures
                .get(0)
                .expect("a regex capture always contains the full match")
        };
        selected = Some(Value::String(value.as_str().to_owned()));
    }
    Ok(selected)
}

fn parse_json_path(pattern: &str) -> Result<JsonPath, String> {
    JsonPath::parse(pattern).map_err(|error| error.to_string())
}

fn scrape_json_path(pattern: &str, input: &str) -> Result<Option<Value>, String> {
    let path = parse_json_path(pattern)?;
    let mut selected = None;
    let documents = serde_json::Deserializer::from_str(input).into_iter::<Value>();
    for (document_index, document) in documents.enumerate() {
        let document = match document {
            Ok(document) => document,
            Err(error) if error.is_eof() && document_index > 0 => break,
            Err(error) => return Err(format!("invalid JSON stream: {error}")),
        };
        for value in path.query(&document).all() {
            if !value.is_null() {
                selected = Some(value.clone());
            }
        }
    }
    Ok(selected)
}

fn read_capture(path: &Path) -> Result<String, AdapterError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path.file_name().ok_or_else(|| {
        capture_read(
            path,
            io::Error::new(io::ErrorKind::InvalidInput, "capture path has no file name"),
        )
    })?;
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(parent)
        .map_err(|source| capture_read(path, source))?;
    let name = CString::new(name.as_bytes()).map_err(|_| {
        capture_read(
            path,
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "capture name contains a NUL byte",
            ),
        )
    })?;
    // The directory descriptor pins the validated capture directory while the
    // final component is opened. O_NONBLOCK makes a replaced FIFO/socket safe
    // to reject with fstat instead of blocking before the type check.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        return Err(capture_read(path, io::Error::last_os_error()));
    }
    // SAFETY: openat returned a new owned descriptor on the successful path.
    let file = unsafe { File::from_raw_fd(descriptor) };
    validate_capture_file(path, &file)?;
    let mut bytes = Vec::new();
    file.take(MAX_CAPTURE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| capture_read(path, source))?;
    if bytes.len() as u64 > MAX_CAPTURE_BYTES {
        return Err(AdapterError::CaptureTooLarge {
            path: path.display().to_string(),
            limit: MAX_CAPTURE_BYTES,
        });
    }
    String::from_utf8(bytes).map_err(|_| AdapterError::CaptureNotUtf8(path.display().to_string()))
}

fn validate_capture_file(path: &Path, file: &File) -> Result<(), AdapterError> {
    let metadata = file
        .metadata()
        .map_err(|source| capture_read(path, source))?;
    if !metadata.file_type().is_file() {
        return Err(AdapterError::CaptureNotRegular(path.display().to_string()));
    }
    if metadata.len() > MAX_CAPTURE_BYTES {
        return Err(AdapterError::CaptureTooLarge {
            path: path.display().to_string(),
            limit: MAX_CAPTURE_BYTES,
        });
    }
    Ok(())
}

fn capture_read(path: &Path, source: io::Error) -> AdapterError {
    AdapterError::CaptureRead {
        path: path.display().to_string(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    use super::*;

    fn adapter() -> AdapterConfig {
        AdapterConfig {
            argv: vec!["agent".to_owned(), "--batch".to_owned()],
            resume: Some(vec![
                "agent".to_owned(),
                "resume".to_owned(),
                "%<sessionRef>%".to_owned(),
                "--context=%<branch>%:%<attempt>%".to_owned(),
                "progress=100%".to_owned(),
            ]),
            scrape: BTreeMap::from([
                (
                    "attempt".to_owned(),
                    ScrapeCapture {
                        stream: ScrapeStream::Stdout,
                        mode: ScrapeMode::JsonPath,
                        pattern: "$..attempt".to_owned(),
                    },
                ),
                (
                    "branch".to_owned(),
                    ScrapeCapture {
                        stream: ScrapeStream::Stderr,
                        mode: ScrapeMode::Regex,
                        pattern: "(?m)^branch=(.+)$".to_owned(),
                    },
                ),
                (
                    "model".to_owned(),
                    ScrapeCapture {
                        stream: ScrapeStream::Stdout,
                        mode: ScrapeMode::JsonPath,
                        pattern: "$..model".to_owned(),
                    },
                ),
                (
                    "sessionRef".to_owned(),
                    ScrapeCapture {
                        stream: ScrapeStream::Stdout,
                        mode: ScrapeMode::JsonPath,
                        pattern: "$..session_id".to_owned(),
                    },
                ),
                (
                    "usage".to_owned(),
                    ScrapeCapture {
                        stream: ScrapeStream::Stdout,
                        mode: ScrapeMode::JsonPath,
                        pattern: "$..usage".to_owned(),
                    },
                ),
            ]),
            yield_hook: Some(vec![
                "tally".to_owned(),
                "lease".to_owned(),
                "status".to_owned(),
            ]),
            env: BTreeMap::from([("AGENT_COLOR".to_owned(), "never".to_owned())]),
            extra_config: BTreeMap::from([(
                "modelFlag".to_owned(),
                Value::String("--model".to_owned()),
            )]),
        }
    }

    fn engine(adapters: &BTreeMap<String, AdapterConfig>) -> AdapterEngine<'_> {
        AdapterEngine::new(adapters)
    }

    #[test]
    fn open_adapter_dispatches_direct_argv_and_multi_capture_resume() {
        let adapters = BTreeMap::from([("custom-nix-agent".to_owned(), adapter())]);
        let engine = engine(&adapters);
        engine.validate_all().unwrap();
        let workload = vec![
            "two words".to_owned(),
            "$(touch /tmp/nope)".to_owned(),
            String::new(),
        ];
        let launch = engine.launch("custom-nix-agent", &workload).unwrap();
        assert_eq!(
            launch.argv,
            ["agent", "--batch", "two words", "$(touch /tmp/nope)", ""]
        );
        assert_eq!(launch.env["AGENT_COLOR"], "never");
        assert_eq!(
            launch.yield_hook.as_deref(),
            Some(&["tally".to_owned(), "lease".to_owned(), "status".to_owned()][..])
        );

        let captures = ScrapeResult {
            captures: BTreeMap::from([
                ("attempt".to_owned(), Value::from(2)),
                ("branch".to_owned(), Value::String("feature/x".to_owned())),
                (
                    "sessionRef".to_owned(),
                    Value::String("sess two".to_owned()),
                ),
            ]),
        };
        let resumed = engine
            .resume("custom-nix-agent", &workload, &captures)
            .unwrap();
        assert_eq!(
            resumed.argv,
            [
                "agent",
                "resume",
                "sess two",
                "--context=feature/x:2",
                "progress=100%",
                "two words",
                "$(touch /tmp/nope)",
                "",
            ]
        );
    }

    #[test]
    fn json_stream_regex_stream_selection_and_verbatim_model_are_pinned() {
        let adapters = BTreeMap::from([("custom".to_owned(), adapter())]);
        let result = engine(&adapters)
            .scrape_text(
                "custom",
                concat!(
                    "{\"event\":{\"session_id\":\"session-1\",\"model\":\"OpenAI/GPT-X.Custom\",\"attempt\":1}}\n",
                    "{\"event\":{\"session_id\":\"session-1\",\"model\":\"OpenAI/GPT-X.Custom\",\"attempt\":2,\"usage\":{\"input_tokens\":17}}}\n"
                ),
                "branch=feature/adapter\n",
            )
            .unwrap();
        assert_eq!(result.session_ref().unwrap(), Some("session-1"));
        assert_eq!(result.model().unwrap(), Some("OpenAI/GPT-X.Custom"));
        assert_eq!(result.captures["attempt"], Value::from(2));
        assert_eq!(result.captures["branch"], "feature/adapter");
        assert_eq!(result.captures["usage"]["input_tokens"], 17);

        let interrupted = engine(&adapters)
            .scrape_text(
                "custom",
                "{\"event\":{\"session_id\":\"recoverable\"}}\n{\"event\":",
                "branch=feature/adapter\n",
            )
            .unwrap();
        assert_eq!(interrupted.session_ref().unwrap(), Some("recoverable"));
        assert!(engine(&adapters)
            .scrape_text(
                "custom",
                "{\"event\":{\"session_id\":\"bad\"}}\nnot-json\n{\"event\":",
                "branch=feature/adapter\n",
            )
            .is_err());

        assert_eq!(
            scrape_json_path(
                "$.events[?@.kind == 'selected'].values[1:]",
                r#"{"events":[{"kind":"ignored","values":[0,1]},{"kind":"selected","values":[2,3,4]}]}"#,
            )
            .unwrap(),
            Some(Value::from(4))
        );
    }

    #[test]
    fn scrape_paths_reads_only_selected_private_capture_streams() {
        let temp = tempfile::tempdir().unwrap();
        let stdout = temp.path().join("job.out");
        let stderr = temp.path().join("job.err");
        fs::write(
            &stdout,
            "{\"event\":{\"session_id\":\"s\",\"model\":\"M\",\"attempt\":1,\"usage\":{}}}\n",
        )
        .unwrap();
        fs::write(&stderr, "branch=main\n").unwrap();
        let paths = ExecutionPaths {
            stdout,
            stderr,
            exit_record: PathBuf::from("unused"),
            capture_generation: PathBuf::from("unused"),
        };
        let adapters = BTreeMap::from([("custom".to_owned(), adapter())]);
        let result = engine(&adapters).scrape_paths("custom", &paths).unwrap();
        assert_eq!(result.captures["branch"], "main");
    }

    #[test]
    fn capture_reader_rejects_links_and_fifos_without_blocking() {
        let temp = tempfile::tempdir().unwrap();
        let regular = temp.path().join("regular");
        fs::write(&regular, "ok").unwrap();
        let link = temp.path().join("link");
        symlink(&regular, &link).unwrap();
        assert!(read_capture(&link).is_err());

        let fifo = temp.path().join("fifo");
        let fifo_name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
        assert!(matches!(
            read_capture(&fifo),
            Err(AdapterError::CaptureNotRegular(_))
        ));

        let real_parent = temp.path().join("real-parent");
        fs::create_dir(&real_parent).unwrap();
        fs::write(real_parent.join("job.out"), "ok").unwrap();
        let linked_parent = temp.path().join("linked-parent");
        symlink(&real_parent, &linked_parent).unwrap();
        assert!(read_capture(&linked_parent.join("job.out")).is_err());
    }

    #[test]
    fn invalid_templates_patterns_environment_and_reserved_capture_types_fail_closed() {
        let mut invalid = adapter();
        invalid.resume = Some(vec!["agent".to_owned(), "%<missing>%".to_owned()]);
        assert!(engine(&BTreeMap::from([("bad".to_owned(), invalid)]))
            .validate_all()
            .unwrap_err()
            .to_string()
            .contains("unknown capture"));

        let mut literal = adapter();
        literal.resume = Some(vec!["agent".to_owned(), "%sessionRef%".to_owned()]);
        let literal_adapters = BTreeMap::from([("literal".to_owned(), literal)]);
        engine(&literal_adapters).validate_all().unwrap();
        assert_eq!(
            engine(&literal_adapters)
                .resume("literal", &[], &ScrapeResult::default())
                .unwrap()
                .argv,
            ["agent", "%sessionRef%"]
        );

        let mut invalid = adapter();
        invalid.resume = Some(vec!["agent".to_owned(), "%<sessionRef".to_owned()]);
        assert!(engine(&BTreeMap::from([("bad".to_owned(), invalid)]))
            .validate_all()
            .unwrap_err()
            .to_string()
            .contains("unclosed"));

        let mut invalid = adapter();
        invalid.scrape.get_mut("branch").unwrap().pattern = "(".to_owned();
        assert!(engine(&BTreeMap::from([("bad".to_owned(), invalid)]))
            .validate_all()
            .is_err());

        let mut invalid = adapter();
        invalid
            .env
            .insert("TALLY_POOL".to_owned(), "stolen".to_owned());
        assert!(engine(&BTreeMap::from([("bad".to_owned(), invalid)]))
            .validate_all()
            .is_err());

        let result = ScrapeResult {
            captures: BTreeMap::from([("model".to_owned(), serde_json::json!({"name": "M"}))]),
        };
        assert!(matches!(
            result.model(),
            Err(AdapterError::ReservedCaptureType(name)) if name == "model"
        ));
    }

    #[test]
    fn missing_resume_capture_and_shell_without_resume_never_fresh_replay() {
        let adapters = BTreeMap::from([
            ("custom".to_owned(), adapter()),
            ("shell".to_owned(), AdapterConfig::default()),
        ]);
        let engine = engine(&adapters);
        engine.validate_all().unwrap();
        assert!(matches!(
            engine.resume("custom", &[], &ScrapeResult::default()),
            Err(AdapterError::MissingCapture { .. })
        ));
        assert!(matches!(
            engine.resume("shell", &["/bin/true".to_owned()], &ScrapeResult::default()),
            Err(AdapterError::NoResume(name)) if name == "shell"
        ));
        assert_eq!(
            engine
                .launch(
                    "shell",
                    &["/bin/echo".to_owned(), "literal;value".to_owned()]
                )
                .unwrap()
                .argv,
            ["/bin/echo", "literal;value"]
        );
    }
}
