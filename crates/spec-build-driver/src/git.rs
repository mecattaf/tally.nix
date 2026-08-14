use std::ffi::OsStr;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::error::{DriverError, Result};

#[derive(Debug)]
pub(crate) struct GitOutput {
    pub(crate) status: i32,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

impl GitOutput {
    pub(crate) fn success(&self) -> bool {
        self.status == 0
    }

    pub(crate) fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub(crate) fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }

    pub(crate) fn stdout_trimmed(&self) -> String {
        self.stdout_text().trim().to_owned()
    }

    pub(crate) fn detail(&self) -> String {
        let stderr = self.stderr_text();
        let stderr = stderr.trim();
        if !stderr.is_empty() {
            return stderr.to_owned();
        }
        let stdout = self.stdout_text();
        let stdout = stdout.trim();
        if !stdout.is_empty() {
            return stdout.to_owned();
        }
        "no output".to_owned()
    }
}

pub(crate) fn git<I, S>(directory: &Path, arguments: I, check: bool) -> Result<GitOutput>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let arguments: Vec<_> = arguments
        .into_iter()
        .map(|argument| argument.as_ref().to_owned())
        .collect();
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(&arguments)
        .output()
        .map_err(|error| DriverError::new(format!("cannot execute git: {error}")))?;
    let result = GitOutput {
        status: output.status.code().unwrap_or(128),
        stdout: output.stdout,
        stderr: output.stderr,
    };
    if check && !result.success() {
        let rendered = arguments
            .iter()
            .map(|argument| argument.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        return Err(DriverError::new(format!(
            "git {rendered} exited {}: {}",
            result.status,
            result.detail()
        )));
    }
    Ok(result)
}

pub(crate) fn git_with_input<I, S>(
    directory: &Path,
    arguments: I,
    input: &[u8],
    check: bool,
) -> Result<GitOutput>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let arguments: Vec<_> = arguments
        .into_iter()
        .map(|argument| argument.as_ref().to_owned())
        .collect();
    let mut child = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(&arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| DriverError::new(format!("cannot execute git: {error}")))?;
    child
        .stdin
        .take()
        .expect("piped Git stdin")
        .write_all(input)
        .map_err(|error| DriverError::new(format!("cannot write git stdin: {error}")))?;
    let output = child
        .wait_with_output()
        .map_err(|error| DriverError::new(format!("cannot wait for git: {error}")))?;
    let result = GitOutput {
        status: output.status.code().unwrap_or(128),
        stdout: output.stdout,
        stderr: output.stderr,
    };
    if check && !result.success() {
        let rendered = arguments
            .iter()
            .map(|argument| argument.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        return Err(DriverError::new(format!(
            "git {rendered} exited {}: {}",
            result.status,
            result.detail()
        )));
    }
    Ok(result)
}
