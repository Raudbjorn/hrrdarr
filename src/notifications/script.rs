//! Custom script execution. No shell, a cleared environment holding only `PATH` plus the event
//! variables, `/` as working directory, closed stdin, bounded output, and a hard timeout that kills
//! the whole process group. The environment contract is documented in `docs/notifications.md`.
use std::{
    io::Read,
    os::unix::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

pub const SCRIPT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_ARGUMENTS: usize = 32;
const MAX_ARGUMENT_BYTES: usize = 1024;
const MAX_CAPTURE_BYTES: usize = 8 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const OUTPUT_GRACE: Duration = Duration::from_millis(500);
const SAFE_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptError {
    InvalidPath,
    InvalidArguments,
    NotExecutable,
    Spawn,
    Timeout,
    Failed,
}
impl ScriptError {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidPath => "script_path_invalid",
            Self::InvalidArguments => "script_arguments_invalid",
            Self::NotExecutable => "script_not_executable",
            Self::Spawn => "script_spawn_failed",
            Self::Timeout => "script_timeout",
            Self::Failed => "script_failed",
        }
    }
}

/// Splits on whitespace; double quotes group a word. No escapes, no expansion, no substitution.
pub fn split_arguments(raw: &str) -> Result<Vec<String>, ScriptError> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut started = false;
    for ch in raw.chars() {
        match ch {
            c if c.is_control() => return Err(ScriptError::InvalidArguments),
            '"' => {
                in_quotes = !in_quotes;
                started = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if started {
                    out.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if in_quotes {
        return Err(ScriptError::InvalidArguments);
    }
    if started {
        out.push(current);
    }
    if out.len() > MAX_ARGUMENTS || out.iter().any(|a| a.len() > MAX_ARGUMENT_BYTES) {
        return Err(ScriptError::InvalidArguments);
    }
    Ok(out)
}

pub fn validate_path(path: &str) -> Result<(), ScriptError> {
    if path.len() > 4096
        || !path.starts_with('/')
        || path.contains('\\')
        || path.chars().any(char::is_control)
        || path.split('/').any(|s| matches!(s, "." | ".."))
    {
        return Err(ScriptError::InvalidPath);
    }
    Ok(())
}

fn capture(mut stream: impl Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    // Keep draining past the cap so the child never blocks on a full pipe.
                    let room = MAX_CAPTURE_BYTES.saturating_sub(kept.len());
                    kept.extend_from_slice(&buffer[..n.min(room)]);
                }
            }
        }
        let _ = tx.send(kept);
    });
    rx
}

pub struct Output {
    pub stdout: String,
    pub stderr: String,
}

/// Blocking; call from `spawn_blocking`.
pub fn run_blocking(
    path: &str,
    arguments: &str,
    env: &[(String, String)],
) -> Result<Output, ScriptError> {
    validate_path(path)?;
    let args = split_arguments(arguments)?;
    let metadata = std::fs::metadata(Path::new(path)).map_err(|_| ScriptError::NotExecutable)?;
    {
        use std::os::unix::fs::PermissionsExt;
        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
            return Err(ScriptError::NotExecutable);
        }
    }
    let mut command = Command::new(path);
    command
        .args(&args)
        .env_clear()
        .env("PATH", SAFE_PATH)
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    for (name, value) in env {
        // Variable names are produced by this crate; values may carry release titles.
        if name.is_empty() || name.contains('=') || name.contains('\0') || value.contains('\0') {
            continue;
        }
        command.env(name, value);
    }
    let mut child = command.spawn().map_err(|_| ScriptError::Spawn)?;
    let out = capture(child.stdout.take().ok_or(ScriptError::Spawn)?);
    let err = capture(child.stderr.take().ok_or(ScriptError::Spawn)?);
    let deadline = Instant::now() + SCRIPT_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => break None,
            Ok(None) => std::thread::sleep(POLL_INTERVAL),
            Err(_) => break None,
        }
    };
    let finished = status.is_some();
    if !finished {
        if let Some(pid) = rustix::process::Pid::from_raw(child.id() as i32) {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::Kill);
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    // A grandchild may keep a pipe open past the leader's exit; do not wait for it forever.
    let stdout = out.recv_timeout(OUTPUT_GRACE).unwrap_or_default();
    let stderr = err.recv_timeout(OUTPUT_GRACE).unwrap_or_default();
    let output = Output {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    };
    match status {
        None => {
            log_output(&output, &mut std::io::stderr());
            Err(ScriptError::Timeout)
        }
        Some(status) => {
            log_output(&output, &mut std::io::stderr());
            if status.success() {
                Ok(output)
            } else {
                Err(ScriptError::Failed)
            }
        }
    }
}

// Script text and paths may contain private event data; log only captured byte counts.
fn log_output(output: &Output, writer: &mut impl std::io::Write) {
    for (level, stream, text) in [
        ("DEBUG", "stdout", &output.stdout),
        ("WARN", "stderr", &output.stderr),
    ] {
        if !text.is_empty() {
            let _ = writeln!(
                writer,
                "{}",
                serde_json::json!({"level":level,"event":"notification_script_output","stream":stream,"captured_bytes":text.len()})
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn script_output_logs_only_diagnostic_counts() {
        let output = Output {
            stdout: "private title /library/private\n".into(),
            stderr: "secret-token\n".into(),
        };
        let mut log = Vec::new();
        log_output(&output, &mut log);
        let records: Vec<serde_json::Value> = String::from_utf8(log)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            records,
            vec![
                serde_json::json!({"level":"DEBUG","event":"notification_script_output","stream":"stdout","captured_bytes":output.stdout.len()}),
                serde_json::json!({"level":"WARN","event":"notification_script_output","stream":"stderr","captured_bytes":output.stderr.len()}),
            ]
        );
    }
    #[test]
    fn arguments_split_without_shell_semantics() {
        assert_eq!(
            split_arguments(r#"--flag "two words" plain  "" $HOME `id` ;ls"#).unwrap(),
            ["--flag", "two words", "plain", "", "$HOME", "`id`", ";ls"]
        );
        assert!(split_arguments("\"unterminated").is_err());
        assert!(split_arguments("a\nb").is_err());
        assert!(split_arguments(&"a ".repeat(33)).is_err());
    }
    #[test]
    fn paths_must_be_absolute_and_normalized() {
        for bad in ["relative.sh", "/a/../b", "/a/./b", "/a\\b", "/a\u{7}b", ""] {
            assert_eq!(validate_path(bad), Err(ScriptError::InvalidPath), "{bad:?}");
        }
        assert!(validate_path("/usr/local/bin/notify.sh").is_ok());
    }
}
