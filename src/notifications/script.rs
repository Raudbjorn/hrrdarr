//! Custom script execution. No shell, a cleared environment holding only `PATH` plus the event
//! variables, `/` as working directory, closed stdin, bounded output, and a hard timeout. The whole
//! process group is killed when the script exits or times out, and output is read without helper
//! threads. The environment contract is documented in `docs/notifications.md`.
use std::{
    io::Read,
    os::unix::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
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

/// One captured stream, read without blocking so no reader thread can outlive the script.
struct Capture<R> {
    stream: Option<R>,
    kept: Vec<u8>,
}
impl<R: Read + std::os::fd::AsFd> Capture<R> {
    fn new(stream: R) -> Result<Self, ScriptError> {
        let flags = rustix::fs::fcntl_getfl(&stream).map_err(|_| ScriptError::Spawn)?;
        rustix::fs::fcntl_setfl(&stream, flags | rustix::fs::OFlags::NONBLOCK)
            .map_err(|_| ScriptError::Spawn)?;
        Ok(Self {
            stream: Some(stream),
            kept: Vec::new(),
        })
    }
    /// Reads whatever is available; the stream is dropped at end of file or on error.
    fn drain(&mut self) {
        let Some(stream) = self.stream.as_mut() else {
            return;
        };
        let mut buffer = [0u8; 4096];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    // Keep draining past the cap so the child never blocks on a full pipe.
                    let room = MAX_CAPTURE_BYTES.saturating_sub(self.kept.len());
                    self.kept.extend_from_slice(&buffer[..n.min(room)]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(_) => break,
            }
        }
        self.stream = None;
    }
    fn open(&self) -> bool {
        self.stream.is_some()
    }
    /// Closes the read end, so a writer that escaped the process group gets EPIPE, not a reader.
    fn finish(mut self) -> String {
        self.stream = None;
        String::from_utf8_lossy(&self.kept).into_owned()
    }
}

/// Whether the leader has exited, without reaping it: the zombie keeps its PID, and so the process
/// group ID, reserved until the group has been signalled.
fn leader_exited(pid: rustix::process::Pid) -> Option<bool> {
    use rustix::process::{WaitId, WaitidOptions, waitid};
    match waitid(
        WaitId::Pid(pid),
        WaitidOptions::EXITED | WaitidOptions::NOHANG | WaitidOptions::NOWAIT,
    ) {
        Ok(status) => Some(status.is_some()),
        Err(_) => None,
    }
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
    let pid = rustix::process::Pid::from_child(&child);
    let pipes = child
        .stdout
        .take()
        .zip(child.stderr.take())
        .ok_or(ScriptError::Spawn)
        .and_then(|(out, err)| Ok((Capture::new(out)?, Capture::new(err)?)));
    let (mut out, mut err) = match pipes {
        Ok(pipes) => pipes,
        Err(e) => {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::Kill);
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
    };
    let deadline = Instant::now() + SCRIPT_TIMEOUT;
    let exited = loop {
        out.drain();
        err.drain();
        match leader_exited(pid) {
            Some(true) => break true,
            Some(false) if Instant::now() < deadline => std::thread::sleep(POLL_INTERVAL),
            _ => break false,
        }
    };
    // Whether the leader finished or timed out, nothing it started outlives the event: work it
    // backgrounded would otherwise accumulate on every delivery. A script that must leave a
    // daemon behind has to detach it (setsid) and close its output.
    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::Kill);
    if !exited {
        let _ = child.kill();
    }
    let status = child.wait().ok().filter(|_| exited);
    // Collect what was already written, bounded; a writer outside the group is cut off.
    let grace = Instant::now() + OUTPUT_GRACE;
    while (out.open() || err.open()) && Instant::now() < grace {
        out.drain();
        err.drain();
        std::thread::sleep(POLL_INTERVAL);
    }
    let output = Output {
        stdout: out.finish(),
        stderr: err.finish(),
    };
    match status {
        None => {
            log_output(path, &output);
            Err(ScriptError::Timeout)
        }
        Some(status) => {
            log_output(path, &output);
            if status.success() {
                Ok(output)
            } else {
                Err(ScriptError::Failed)
            }
        }
    }
}

/// Script stdout is DEBUG and stderr is WARN for both domains: Sonarr logs stderr as an error and
/// Radarr as information, so hrrdarr picks one consistent middle level instead of copying either.
fn log_output(path: &str, output: &Output) {
    for (level, stream, text) in [
        ("DEBUG", "stdout", &output.stdout),
        ("WARN", "stderr", &output.stderr),
    ] {
        let text = text.trim();
        if !text.is_empty() {
            let clipped: String = text.chars().take(2048).collect();
            eprintln!(
                "{}",
                serde_json::json!({"level":level,"event":"notification_script_output","script":path,"stream":stream,"text":clipped})
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    /// A process is gone once it no longer exists or is only a zombie awaiting its reaper.
    fn gone(pid: &str) -> bool {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => true,
            Ok(stat) => stat
                .rsplit_once(") ")
                .is_some_and(|(_, rest)| rest.starts_with('Z')),
        }
    }
    #[test]
    fn exit_kills_backgrounded_work_and_returns_output_promptly() {
        let dir = std::env::temp_dir().join(format!("hrrdarr-script-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let script = dir.join("hook.sh");
        let pid_file = dir.join("pid");
        // The background sleep inherits stdout/stderr, which used to pin two reader threads, and
        // stays in the script's process group, which used to survive a successful exit.
        std::fs::write(
            &script,
            "#!/bin/sh\necho visible\necho warned >&2\nsleep 30 &\necho $! > \"$1\"\nexit 0\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let started = Instant::now();
        let output = run_blocking(
            script.to_str().unwrap(),
            &format!("\"{}\"", pid_file.display()),
            &[],
        )
        .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(output.stdout.trim(), "visible");
        assert_eq!(output.stderr.trim(), "warned");
        let pid = std::fs::read_to_string(&pid_file).unwrap();
        let pid = pid.trim();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !gone(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(gone(pid), "backgrounded process {pid} survived the script");
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn failing_scripts_report_failure_with_output() {
        let dir = std::env::temp_dir().join(format!("hrrdarr-script-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let script = dir.join("fail.sh");
        std::fs::write(&script, "#!/bin/sh\necho before-failure >&2\nexit 3\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(
            run_blocking(script.to_str().unwrap(), "", &[]).err(),
            Some(ScriptError::Failed)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn paths_must_be_absolute_and_normalized() {
        for bad in ["relative.sh", "/a/../b", "/a/./b", "/a\\b", "/a\u{7}b", ""] {
            assert_eq!(validate_path(bad), Err(ScriptError::InvalidPath), "{bad:?}");
        }
        assert!(validate_path("/usr/local/bin/notify.sh").is_ok());
    }
}
