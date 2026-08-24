use agentd::model::{ActivityState, CwdState, Harness, Snapshot};
use serde::Deserialize;
use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "agentd-{label}-{}-{}",
            std::process::id(),
            monotonic_suffix()
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Daemon {
    child: Child,
    socket: PathBuf,
}

impl Daemon {
    fn start(runtime: &Path) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_agentd"))
            .arg("daemon")
            .env("XDG_RUNTIME_DIR", runtime)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let socket = runtime.join("agentd.sock");
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut ready = false;
        while Instant::now() < deadline {
            if let Ok(metadata) = fs::symlink_metadata(&socket)
                && metadata.permissions().mode() & 0o777 == 0o600
                && UnixStream::connect(&socket).is_ok()
            {
                ready = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(ready, "daemon socket did not become ready");
        let mode = fs::symlink_metadata(&socket).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        Self { child, socket }
    }

    fn stop(mut self) {
        let result = unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM) };
        assert_eq!(result, 0);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "daemon did not exit 0: {status}");
                break;
            }
            assert!(
                Instant::now() < deadline,
                "daemon did not stop within five seconds"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!self.socket.exists(), "daemon socket survived shutdown");
    }
}

#[test]
fn protocol_byte_limit_and_closed_errors() {
    let runtime = TestDir::new("protocol");
    let daemon = Daemon::start(&runtime.0);

    let snapshot = request(&daemon.socket, b"{\"version\":1,\"op\":\"snapshot\"}\n");
    let decoded: Snapshot = serde_json::from_slice(&snapshot).unwrap();
    assert!(decoded.revision >= 1);

    assert_error(
        &request(&daemon.socket, b"{\"version\":2,\"op\":\"snapshot\"}\n"),
        "unsupported_version",
    );
    assert_error(
        &request(&daemon.socket, b"{\"version\":1,\"op\":\"history\"}\n"),
        "unknown_operation",
    );
    assert_error(&request(&daemon.socket, b"not json\n"), "malformed_request");

    let mut at_limit = vec![b' '; 65_536];
    at_limit.push(b'\n');
    assert_error(&request(&daemon.socket, &at_limit), "malformed_request");

    let oversized = vec![b' '; 65_537];
    assert_error(&request(&daemon.socket, &oversized), "request_too_large");
    daemon.stop();
}

#[test]
fn real_procfs_roster_stream_activity_and_exit_deadline() {
    let runtime = TestDir::new("runtime");
    let shared_cwd = TestDir::new("shared-cwd");
    let commands = TestDir::new("commands");
    symlink("/bin/sleep", commands.0.join("codex")).unwrap();
    symlink("/bin/sleep", commands.0.join("claude")).unwrap();
    let daemon = Daemon::start(&runtime.0);
    let mut subscription = UnixStream::connect(&daemon.socket).unwrap();
    subscription
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    subscription
        .write_all(b"{\"version\":1,\"op\":\"subscribe\"}\n")
        .unwrap();
    let mut reader = BufReader::new(subscription);
    let _: Snapshot = read_snapshot(&mut reader);

    let start = Instant::now();
    spawn_named(
        &commands.0.join("codex"),
        &shared_cwd.0,
        "CODEX_ONE_SENTINEL",
    );
    spawn_named(
        &commands.0.join("codex"),
        &shared_cwd.0,
        "CODEX_TWO_SENTINEL",
    );
    spawn_named(
        &commands.0.join("codex"),
        &shared_cwd.0,
        "CODEX_THREE_SENTINEL",
    );
    spawn_named(&commands.0.join("claude"), &shared_cwd.0, "CLAUDE_SENTINEL");
    let present = wait_for_snapshot(&mut reader, |snapshot| {
        matching_agents(snapshot, &shared_cwd.0).len() == 4
    });
    assert!(start.elapsed() <= Duration::from_secs(2));
    let matching = matching_agents(&present, &shared_cwd.0);
    assert_eq!(
        matching
            .iter()
            .filter(|agent| agent.harness == Harness::Codex)
            .count(),
        3
    );
    assert_eq!(
        matching
            .iter()
            .filter(|agent| agent.harness == Harness::Claude)
            .count(),
        1
    );
    assert!(
        matching
            .iter()
            .all(|agent| agent.activity.state == ActivityState::Unknown)
    );
    let encoded = serde_json::to_string(&present).unwrap();
    for sentinel in [
        "CODEX_ONE_SENTINEL",
        "CODEX_TWO_SENTINEL",
        "CODEX_THREE_SENTINEL",
        "CLAUDE_SENTINEL",
    ] {
        assert!(!encoded.contains(sentinel));
    }

    let target = matching[0];
    let wrong_identity = format!(
        "{{\"version\":1,\"op\":\"activity\",\"agent\":{{\"pid\":{},\"startTimeTicks\":{}}},\"state\":\"active\"}}\n",
        target.id.pid,
        target.id.start_time_ticks + 1
    );
    assert_error(
        &request(&daemon.socket, wrong_identity.as_bytes()),
        "unknown_agent",
    );
    let unchanged: Snapshot = serde_json::from_slice(&request(
        &daemon.socket,
        b"{\"version\":1,\"op\":\"snapshot\"}\n",
    ))
    .unwrap();
    assert_eq!(
        unchanged
            .agents
            .iter()
            .find(|agent| agent.id == target.id)
            .unwrap()
            .activity
            .state,
        ActivityState::Unknown
    );

    let output = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args([
            "activity",
            "--pid",
            &target.id.pid.to_string(),
            "--state",
            "active",
        ])
        .env("XDG_RUNTIME_DIR", &runtime.0)
        .output()
        .unwrap();
    assert_silent_success(output);
    let changed = wait_for_snapshot(&mut reader, |snapshot| {
        snapshot
            .agents
            .iter()
            .any(|agent| agent.id == target.id && agent.activity.state == ActivityState::Active)
    });
    assert_eq!(
        changed.reason,
        agentd::model::SnapshotReason::ActivityChanged
    );

    for agent in matching {
        let result = unsafe { libc::kill(agent.id.pid as libc::pid_t, libc::SIGTERM) };
        assert_eq!(result, 0);
    }
    let exit_start = Instant::now();
    let gone = wait_for_snapshot(&mut reader, |snapshot| {
        matching_agents(snapshot, &shared_cwd.0).is_empty()
    });
    assert!(exit_start.elapsed() <= Duration::from_secs(2));
    assert!(matching_agents(&gone, &shared_cwd.0).is_empty());
    daemon.stop();
}

#[test]
fn regular_file_at_socket_path_is_refused_without_removal() {
    let runtime = TestDir::new("regular-path");
    let socket = runtime.0.join("agentd.sock");
    fs::write(&socket, b"do not remove").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .arg("daemon")
        .env("XDG_RUNTIME_DIR", &runtime.0)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::read(&socket).unwrap(), b"do not remove");
    assert!(String::from_utf8_lossy(&output.stderr).contains("refusing to remove non-socket path"));
}

#[test]
fn stale_socket_is_replaced_but_live_listener_is_preserved() {
    let stale_runtime = TestDir::new("stale-socket");
    let stale_path = stale_runtime.0.join("agentd.sock");
    let stale = UnixListener::bind(&stale_path).unwrap();
    drop(stale);
    assert_eq!(
        UnixStream::connect(&stale_path).unwrap_err().kind(),
        std::io::ErrorKind::ConnectionRefused
    );
    let daemon = Daemon::start(&stale_runtime.0);
    daemon.stop();

    let live_runtime = TestDir::new("live-socket");
    let live_path = live_runtime.0.join("agentd.sock");
    let listener = UnixListener::bind(&live_path).unwrap();
    let live_inode = fs::symlink_metadata(&live_path).unwrap().ino();
    let output = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .arg("daemon")
        .env("XDG_RUNTIME_DIR", &live_runtime.0)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::symlink_metadata(&live_path).unwrap().ino(), live_inode);
    assert!(String::from_utf8_lossy(&output.stderr).contains("live listener already owns"));
    drop(listener);
}

#[test]
fn systemd_unit_has_fixed_user_lifecycle_contract() {
    let unit = fs::read_to_string("packaging/systemd/agentd.service").unwrap();
    assert!(unit.contains("ExecStart=%h/.local/bin/agentd daemon"));
    assert!(unit.contains("Restart=on-failure"));
    assert!(unit.contains("WantedBy=default.target"));
    assert!(!unit.contains("--socket"));
}

#[test]
fn captured_real_procfs_fixtures_replay_parser_fields() {
    let fixture: ProcFixture =
        serde_json::from_str(include_str!("fixtures/real-procfs-2026-08-24.json")).unwrap();
    assert_eq!(fixture.processes.len(), 4);
    for process in fixture.processes {
        let stat = agentd::procfs::parse_stat(&process.stat, process.pid).unwrap();
        assert_eq!(stat.parent_pid, process.parent_pid);
        assert_eq!(stat.start_time_ticks, process.start_time_ticks);
        assert_eq!(
            agentd::model::Harness::from_comm(&stat.comm)
                .unwrap()
                .as_str(),
            process.harness
        );
        assert_eq!(
            agentd::procfs::parse_effective_uid(&process.status_uid).unwrap(),
            process.effective_uid
        );
        assert!(Path::new(&process.cwd).is_absolute());
    }
}

fn request(path: &Path, bytes: &[u8]) -> Vec<u8> {
    let mut stream = UnixStream::connect(path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream.write_all(bytes).unwrap();
    let mut reader = BufReader::new(stream);
    let mut frame = Vec::new();
    reader.read_until(b'\n', &mut frame).unwrap();
    frame
}

fn assert_error(frame: &[u8], code: &str) {
    let value: Value = serde_json::from_slice(frame).unwrap();
    assert_eq!(value.get("type").and_then(Value::as_str), Some("error"));
    assert_eq!(value.get("code").and_then(Value::as_str), Some(code));
    assert_eq!(value.as_object().unwrap().len(), 3);
}

fn read_snapshot(reader: &mut BufReader<UnixStream>) -> Snapshot {
    let mut frame = Vec::new();
    reader.read_until(b'\n', &mut frame).unwrap();
    serde_json::from_slice(&frame).unwrap()
}

fn wait_for_snapshot(
    reader: &mut BufReader<UnixStream>,
    predicate: impl Fn(&Snapshot) -> bool,
) -> Snapshot {
    loop {
        let snapshot = read_snapshot(reader);
        if predicate(&snapshot) {
            return snapshot;
        }
    }
}

fn matching_agents<'a>(snapshot: &'a Snapshot, cwd: &Path) -> Vec<&'a agentd::model::AgentRecord> {
    let cwd = cwd.to_string_lossy();
    snapshot
        .agents
        .iter()
        .filter(|agent| {
            agent.cwd.state == CwdState::Known && agent.cwd.value.as_deref() == Some(cwd.as_ref())
        })
        .collect()
}

fn spawn_named(command: &Path, cwd: &Path, sentinel: &str) {
    let status = Command::new("setsid")
        .arg("--fork")
        .arg(command)
        .arg("30")
        .current_dir(cwd)
        .env("AGENTD_PRIVACY_SENTINEL", sentinel)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
}

fn assert_silent_success(output: Output) {
    assert!(output.status.success(), "activity failed: {output:?}");
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

fn monotonic_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProcFixture {
    processes: Vec<FixtureProcess>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureProcess {
    pid: u32,
    stat: String,
    status_uid: String,
    cwd: String,
    harness: String,
    parent_pid: u32,
    start_time_ticks: u64,
    effective_uid: u32,
}
