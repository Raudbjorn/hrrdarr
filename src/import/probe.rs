//! Read-only measurement; invoke from a bounded blocking worker, never an async executor.
use super::{Result, scan_fs::Inventory};
use crate::media_files::probe::{ProbedMedia, parse};
use rustix::{
    fs::{OFlags, fcntl_getfl, fcntl_setfl},
    process::{Resource, Rlimit, getrlimit, setrlimit},
};
use std::{
    fs::File,
    io::{ErrorKind, Read},
    os::unix::process::CommandExt,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const MAX_OUTPUT_BYTES: usize = 256 * 1024;
// RLIMIT_AS counts every mapped library, not only media work. Measured on ffmpeg-full 9.0.2 (369
// DT_NEEDED libraries, including the SVT encoders): `ffprobe -version` alone needs ~1.4 GiB of
// address space and the fixture probes add no measurable amount (peak RSS ~63 MiB), so a 1 GiB cap
// can never start that build. 3 GiB leaves ~2x loader headroom while ffmpeg's own `-max_alloc`,
// one thread, and the CPU/wall/output limits still bound the media work. Host-specific figures.
const ADDRESS_SPACE_BYTES: u64 = 3 * 1024 * 1024 * 1024;
// The dynamic loader exits with 127 when a required shared object cannot be found or mapped.
const LOADER_EXIT_CODE: i32 = 127;
const LOADER_FAILED: &str = "probe_loader_failed";
const CAPABILITY_WALL_TIME: Duration = Duration::from_secs(5);
const MAX_WALL_TIME: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(5);
// Only self-contained demuxers: no concat, playlists, image sequences or network protocols.
const FORMATS: &str = "matroska,webm,mov,mp4,m4a,3gp,3g2,mj2,avi,mpeg,mpegts,mpegvideo,m4v,h264,flv,asf,ogg,rm,wtv,ivf,wav,nsv,ty,pva,nuv,vivo,dv,flic";

#[derive(Debug)]
pub(crate) enum Observation {
    Measured(ProbedMedia),
    Unavailable(&'static str),
}

pub(crate) fn observe(
    inventory: &Inventory,
    index: usize,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<Observation> {
    observe_with(inventory, index, deadline, cancel, &mut command())
}

fn observe_with(
    inventory: &Inventory,
    index: usize,
    deadline: Instant,
    cancel: &AtomicBool,
    command: &mut Command,
) -> Result<Observation> {
    let deadline = deadline.min(Instant::now() + MAX_WALL_TIME);
    if let Err(reason) = check_budget(deadline, cancel) {
        return Ok(Observation::Unavailable(reason));
    }
    let held = inventory.open_file(index)?;
    let measured = run(command, &held, deadline, cancel)
        .map_err(persisted_reason)
        .and_then(|raw| parse(&raw));
    // A probe failure cannot hide a replaced path or modified inode.
    inventory.verify_open_file(index, &held)?;
    // Descriptor syscalls cannot be interrupted, but late observations are never published.
    Ok(match check_budget(deadline, cancel).and(measured) {
        Ok(value) => Observation::Measured(value),
        Err(reason) => Observation::Unavailable(reason),
    })
}

/// Reasons are stored in `downloaded_scan_children.decision_json`, whose published migration 0057
/// accepts a closed set. Loader failure is surfaced by [`capability`], not as a new stored reason.
fn persisted_reason(reason: &'static str) -> &'static str {
    if reason == LOADER_FAILED {
        "probe_failed"
    } else {
        reason
    }
}

/// Result of starting the installed `ffprobe` under the production resource limits. Carries no
/// path, version text or child output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Capability {
    Ready,
    Missing,
    LoaderFailed,
    Failed,
}

impl Capability {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Missing => "executable_missing",
            Self::LoaderFailed => "loader_failed",
            Self::Failed => "start_failed",
        }
    }
}

/// Startup diagnostic; invoke from a blocking worker. Distinguishes an absent executable, an
/// installed binary the resource policy prevents from loading, and a working one.
pub(crate) fn capability(cancel: &AtomicBool) -> Capability {
    let mut command = Command::new("ffprobe");
    command.arg("-version").env_remove("FFREPORT");
    capability_with(&mut command, Instant::now() + CAPABILITY_WALL_TIME, cancel)
}

fn capability_with(command: &mut Command, deadline: Instant, cancel: &AtomicBool) -> Capability {
    let Ok(null) = File::open("/dev/null") else {
        return Capability::Failed;
    };
    match run(command, &null, deadline, cancel) {
        Ok(out) if out.starts_with(b"ffprobe version") => Capability::Ready,
        Err("probe_executable_missing") => Capability::Missing,
        Err(LOADER_FAILED) => Capability::LoaderFailed,
        _ => Capability::Failed,
    }
}

fn command() -> Command {
    let mut command = Command::new("ffprobe");
    command.args([
        "-v", "error", "-max_alloc", "67108864",
        "-probesize", "5000000", "-analyzeduration", "5000000",
        "-max_probe_packets", "2500", "-threads", "1",
        "-protocol_whitelist", "file", "-format_whitelist", FORMATS,
        "-enable_drefs", "0", "-use_absolute_path", "0",
        "-show_entries",
        "format=format_name,duration:stream=codec_type,codec_name,codec_tag_string,profile,width,height,bit_rate,bits_per_raw_sample,avg_frame_rate,r_frame_rate,channels,channel_layout,field_order,color_transfer,color_primaries,color_space:stream_tags=language:stream_disposition=attached_pic,forced,hearing_impaired:stream_side_data",
        "-of", "json", "-i", "/proc/self/fd/0",
    ]);
    // Do not let ffprobe's environment-driven report feature write private media facts.
    command.env_remove("FFREPORT");
    command
}

fn check_budget(deadline: Instant, cancel: &AtomicBool) -> std::result::Result<(), &'static str> {
    if cancel.load(Ordering::Acquire) {
        Err("probe_cancelled")
    } else if Instant::now() >= deadline {
        Err("probe_timeout")
    } else {
        Ok(())
    }
}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        // Unwind fallback. Normal completion explicitly checks kill/reap errors.
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn run(
    command: &mut Command,
    file: &File,
    deadline: Instant,
    cancel: &AtomicBool,
) -> std::result::Result<Vec<u8>, &'static str> {
    let deadline = deadline.min(Instant::now() + MAX_WALL_TIME);
    check_budget(deadline, cancel)?;
    command
        .stdin(Stdio::from(
            file.try_clone().map_err(|_| "probe_descriptor_failed")?,
        ))
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // SAFETY: after fork this closure uses only direct rlimit syscalls and stack values.
    unsafe {
        command.pre_exec(|| {
            for (resource, cap) in [
                (Resource::Cpu, 10),
                (Resource::As, ADDRESS_SPACE_BYTES),
                (Resource::Nofile, 64),
                (Resource::Core, 0),
                (Resource::Fsize, 0),
            ] {
                let old = getrlimit(resource);
                let cap = cap
                    .min(old.current.unwrap_or(cap))
                    .min(old.maximum.unwrap_or(cap));
                setrlimit(
                    resource,
                    Rlimit {
                        current: Some(cap),
                        maximum: Some(cap),
                    },
                )?;
            }
            Ok(())
        });
    }
    let mut child = OwnedChild(command.spawn().map_err(|e| {
        if e.kind() == ErrorKind::NotFound {
            "probe_executable_missing"
        } else {
            "probe_spawn_failed"
        }
    })?);
    collect(&mut child.0, deadline, cancel)
}

fn collect(
    child: &mut Child,
    deadline: Instant,
    cancel: &AtomicBool,
) -> std::result::Result<Vec<u8>, &'static str> {
    let result = read_output(child, deadline, cancel);
    match child.try_wait().map_err(|_| "probe_reap_failed")? {
        Some(_) => (),
        None => {
            child.kill().map_err(|_| "probe_kill_failed")?;
            // shortcut: SIGKILL/reap cannot preempt uninterruptible kernel I/O; use an
            // external supervisor if a hard host-level deadline becomes necessary.
            child.wait().map_err(|_| "probe_reap_failed")?;
        }
    }
    result
}

fn read_output(
    child: &mut Child,
    deadline: Instant,
    cancel: &AtomicBool,
) -> std::result::Result<Vec<u8>, &'static str> {
    let mut stdout = child.stdout.take().ok_or("probe_output_missing")?;
    let flags = fcntl_getfl(&stdout).map_err(|_| "probe_output_failed")?;
    fcntl_setfl(&stdout, flags | OFlags::NONBLOCK).map_err(|_| "probe_output_failed")?;
    let mut output = Vec::new();
    let mut buffer = [0; 8192];
    let mut eof = false;
    loop {
        check_budget(deadline, cancel)?;
        if !eof {
            match stdout.read(&mut buffer) {
                Ok(0) => eof = true,
                Ok(n) => {
                    if n > MAX_OUTPUT_BYTES - output.len() {
                        return Err("probe_output_limit");
                    }
                    output.extend_from_slice(&buffer[..n]);
                    continue;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => (),
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(_) => return Err("probe_output_failed"),
            }
        }
        if let Some(status) = child.try_wait().map_err(|_| "probe_wait_failed")? {
            if !status.success() {
                return Err(if status.code() == Some(LOADER_EXIT_CODE) {
                    LOADER_FAILED
                } else {
                    "probe_failed"
                });
            }
            if eof {
                return Ok(output);
            }
        }
        std::thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("hrrdarr-probe-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }
        fn inventory(&self, bytes: &[u8]) -> Inventory {
            let path = self.0.join("media.wav");
            fs::write(&path, bytes).unwrap();
            Inventory::capture(path.to_str().unwrap()).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn shell(script: &str) -> Command {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", script]);
        cmd
    }
    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    #[test]
    fn failures_are_unavailable_and_changed_sources_remain_errors() {
        let fixture = Fixture::new();
        let inventory = fixture.inventory(b"not media");
        let cancel = AtomicBool::new(false);
        for (mut cmd, reason) in [
            (
                Command::new(fixture.0.join("absent")),
                "probe_executable_missing",
            ),
            (shell("exit 7"), "probe_failed"),
            (shell("printf broken"), "probe_json_invalid"),
            (shell("printf '{}'"), "probe_streams_missing"),
        ] {
            assert!(
                matches!(observe_with(&inventory, 0, deadline(), &cancel, &mut cmd).unwrap(),
                Observation::Unavailable(actual) if actual == reason)
            );
        }
        let mut cmd = shell("printf '{\"streams\":[]}'");
        assert!(
            matches!(observe_with(&inventory, 0, deadline(), &cancel, &mut cmd).unwrap(),
            Observation::Measured(value) if value.audio_stream_count == 0 && value.runtime_ticks.is_none())
        );
        let mut cmd = shell("printf changed > \"$1\"; exit 7");
        cmd.arg("probe-test").arg(fixture.0.join("media.wav"));
        assert!(observe_with(&inventory, 0, deadline(), &cancel, &mut cmd).is_err());
    }

    #[test]
    fn capability_separates_missing_loader_failure_and_working_binary() {
        let fixture = Fixture::new();
        let cancel = AtomicBool::new(false);
        // Child stderr is discarded, so a loader message naming private paths cannot be echoed.
        for (mut cmd, expected) in [
            (Command::new(fixture.0.join("absent")), Capability::Missing),
            (
                shell(
                    "echo '/private/media.mkv: error while loading shared libraries' >&2; exit 127",
                ),
                Capability::LoaderFailed,
            ),
            (shell("exit 1"), Capability::Failed),
            (shell("printf unrelated"), Capability::Failed),
            (shell("printf 'ffprobe version test'"), Capability::Ready),
        ] {
            assert_eq!(capability_with(&mut cmd, deadline(), &cancel), expected);
        }
        let codes: Vec<_> = [
            Capability::Ready,
            Capability::Missing,
            Capability::LoaderFailed,
            Capability::Failed,
        ]
        .map(Capability::code)
        .into();
        assert_eq!(
            codes,
            [
                "ready",
                "executable_missing",
                "loader_failed",
                "start_failed"
            ]
        );
    }

    #[test]
    fn loader_failure_is_stored_as_the_published_probe_failed_reason() {
        let fixture = Fixture::new();
        let inventory = fixture.inventory(b"unused");
        let cancel = AtomicBool::new(false);
        assert_eq!(
            run(
                &mut shell("exit 127"),
                &inventory.open_file(0).unwrap(),
                deadline(),
                &cancel
            ),
            Err(LOADER_FAILED)
        );
        assert!(matches!(
            observe_with(&inventory, 0, deadline(), &cancel, &mut shell("exit 127")).unwrap(),
            Observation::Unavailable("probe_failed")
        ));
    }

    #[test]
    fn expired_or_cancelled_observation_never_spawns() {
        let fixture = Fixture::new();
        let inventory = fixture.inventory(b"unused");
        let marker = fixture.0.join("must-not-exist");
        for (until, cancel, expected) in [
            (Instant::now(), false, "probe_timeout"),
            (deadline(), true, "probe_cancelled"),
        ] {
            let mut cmd = shell("printf spawned > \"$1\"");
            cmd.arg("probe-test").arg(&marker);
            assert!(
                matches!(observe_with(&inventory, 0, until, &AtomicBool::new(cancel), &mut cmd).unwrap(),
                Observation::Unavailable(reason) if reason == expected)
            );
            assert!(!marker.exists());
        }
    }

    #[test]
    fn timeout_cancellation_and_output_limit_reap_the_child() {
        for (script, budget, cancel, expected) in [
            (
                "exec sleep 60",
                Duration::from_millis(40),
                false,
                "probe_timeout",
            ),
            (
                "exec sleep 60",
                Duration::from_secs(2),
                true,
                "probe_cancelled",
            ),
            (
                "while :; do printf '12345678901234567890123456789012'; done",
                Duration::from_secs(3),
                false,
                "probe_output_limit",
            ),
        ] {
            let mut child = OwnedChild(
                shell(script)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            assert_eq!(
                collect(
                    &mut child.0,
                    Instant::now() + budget,
                    &AtomicBool::new(cancel)
                ),
                Err(expected)
            );
            assert!(child.0.try_wait().unwrap().is_some());
        }
    }

    #[test]
    fn child_limits_do_not_change_parent_and_playlist_cannot_connect() {
        let fixture = Fixture::new();
        let inventory = fixture.inventory(b"unused");
        let file = inventory.open_file(0).unwrap();
        let before = getrlimit(Resource::Nofile);
        let raw = run(
            &mut shell("ulimit -c; ulimit -f; ulimit -n; ulimit -v"),
            &file,
            deadline(),
            &AtomicBool::new(false),
        )
        .unwrap();
        // `ulimit -v` reports KiB: the address-space cap stays finite (see ADDRESS_SPACE_BYTES).
        assert_eq!(
            String::from_utf8(raw).unwrap(),
            format!("0\n0\n64\n{}\n", ADDRESS_SPACE_BYTES / 1024)
        );
        assert_eq!(getrlimit(Resource::Nofile), before);

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let playlist = format!(
            "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\nhttp://{}/segment.ts\n#EXT-X-ENDLIST\n",
            listener.local_addr().unwrap()
        );
        let inventory = fixture.inventory(playlist.as_bytes());
        let observation = observe(&inventory, 0, deadline(), &AtomicBool::new(false)).unwrap();
        assert!(matches!(
            observation,
            Observation::Unavailable("probe_failed" | "probe_executable_missing")
        ));
        assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    }

    #[test]
    fn descriptor_input_and_real_ffprobe_measurement() {
        let fixture = Fixture::new();
        let samples = 8000_u32;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + samples * 2).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&8000_u32.to_le_bytes());
        wav.extend_from_slice(&16000_u32.to_le_bytes());
        wav.extend_from_slice(&2_u16.to_le_bytes());
        wav.extend_from_slice(&16_u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(samples * 2).to_le_bytes());
        wav.resize(wav.len() + samples as usize * 2, 0);
        let inventory = fixture.inventory(&wav);
        let cancel = AtomicBool::new(false);
        // An installed ffprobe the resource policy cannot start must fail here, not be skipped.
        let capability = capability(&cancel);
        assert!(
            matches!(capability, Capability::Ready | Capability::Missing),
            "installed ffprobe is unusable under the probe limits: {}",
            capability.code()
        );
        let observed = observe(&inventory, 0, deadline(), &cancel).unwrap();
        match observed {
            Observation::Measured(media) => {
                assert_eq!(media.audio_stream_count, 1);
                assert_eq!(media.runtime_ticks, Some(10_000_000));
                assert!(media.video_width.is_none());
                assert!(media.video_height.is_none());
                for domain in ["tv", "movies"] {
                    media.normalized_info(domain).unwrap();
                }
            }
            Observation::Unavailable("probe_executable_missing") => {
                eprintln!("ffprobe is not installed; real-media observation not exercised");
            }
            other => panic!("unexpected observation: {other:?}"),
        }
        let inventory = fixture.inventory(include_bytes!("../../tests/fixtures/probe-video.webm"));
        match observe(&inventory, 0, deadline(), &cancel).unwrap() {
            Observation::Measured(media) => {
                assert_eq!(media.audio_stream_count, 0);
                assert_eq!(media.runtime_ticks, Some(10_000_000));
                assert_eq!(
                    (media.video_width, media.video_height),
                    (Some(16), Some(16))
                );
                for domain in ["tv", "movies"] {
                    media.normalized_info(domain).unwrap();
                }
            }
            Observation::Unavailable("probe_executable_missing") => {
                eprintln!("ffprobe is not installed; real video observation not exercised");
            }
            other => panic!("unexpected video observation: {other:?}"),
        }
    }
}
