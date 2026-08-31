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
            .env("XDG_STATE_HOME", runtime.join("state"))
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

    let output = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args([
            "activity",
            "--pid",
            &target.id.pid.to_string(),
            "--state",
            "needs_attention",
        ])
        .env("XDG_RUNTIME_DIR", &runtime.0)
        .output()
        .unwrap();
    assert_silent_success(output);
    let attention = wait_for_snapshot(&mut reader, |snapshot| {
        snapshot.agents.iter().any(|agent| {
            agent.id == target.id && agent.activity.state == ActivityState::NeedsAttention
        })
    });
    assert_eq!(
        attention.reason,
        agentd::model::SnapshotReason::ActivityChanged
    );
    let attention_frame = request(&daemon.socket, b"{\"version\":1,\"op\":\"snapshot\"}\n");
    let attention_json: Value = serde_json::from_slice(&attention_frame).unwrap();
    let attention_agent = attention_json["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| {
            agent["id"]["pid"].as_u64() == Some(u64::from(target.id.pid))
                && agent["id"]["startTimeTicks"].as_u64() == Some(target.id.start_time_ticks)
        })
        .unwrap();
    assert_eq!(
        attention_agent["activity"]["state"].as_str(),
        Some("needs_attention")
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
        .env("XDG_STATE_HOME", runtime.0.join("state"))
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
    wait_for_connection_refused(&stale_path);
    let daemon = Daemon::start(&stale_runtime.0);
    daemon.stop();

    let live_runtime = TestDir::new("live-socket");
    let live_path = live_runtime.0.join("agentd.sock");
    let listener = UnixListener::bind(&live_path).unwrap();
    let live_inode = fs::symlink_metadata(&live_path).unwrap().ino();
    let output = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .arg("daemon")
        .env("XDG_RUNTIME_DIR", &live_runtime.0)
        .env("XDG_STATE_HOME", live_runtime.0.join("state"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::symlink_metadata(&live_path).unwrap().ino(), live_inode);
    assert!(String::from_utf8_lossy(&output.stderr).contains("live listener already owns"));
    drop(listener);
}

fn wait_for_connection_refused(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match UnixStream::connect(path) {
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => return,
            Ok(stream) => drop(stream),
            Err(error) => panic!("unexpected stale-socket probe error: {error}"),
        }
        assert!(
            Instant::now() < deadline,
            "stale listener remained connectable after its owner closed"
        );
        thread::sleep(Duration::from_millis(1));
    }
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

#[test]
fn hook_adapter_discards_payload_and_fails_open_with_a_typed_diagnostic() {
    let started = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args([
            "hook",
            "--integration",
            "agentd-v1.1",
            "--harness",
            "claude",
            "--event",
            "Notification",
        ])
        .env_remove("XDG_RUNTIME_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"PRIVATE_PROMPT_AND_TOOL_INPUT")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "hook did not fail open: {output:?}"
    );
    assert!(started.elapsed() < Duration::from_millis(750));
    assert!(output.stdout.is_empty());
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    assert_eq!(diagnostic, "agentd hook: ancestry_unresolved\n");
    assert!(!diagnostic.contains("PRIVATE_PROMPT_AND_TOOL_INPUT"));

    let invalid = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args([
            "hook",
            "--integration",
            "agentd-v1.1",
            "--harness",
            "codex",
            "--event",
            "Notification",
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    assert!(invalid.stdout.is_empty());
    assert_eq!(
        String::from_utf8(invalid.stderr).unwrap(),
        "agentd hook: invalid_hook_event\n"
    );

    let mut name = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args(["name", "--from-claude-session-start", "Review lane"])
        .env_remove("XDG_RUNTIME_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    name.stdin
        .take()
        .unwrap()
        .write_all(b"PRIVATE_SESSION_START_SENTINEL")
        .unwrap();
    let output = name.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "name hook did not fail open: {output:?}"
    );
    assert!(output.stdout.is_empty());
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    assert_eq!(diagnostic, "agentd name: ancestry_unresolved\n");
    assert!(!diagnostic.contains("PRIVATE_SESSION_START_SENTINEL"));
}

#[test]
fn name_set_restart_clear_and_stale_identity_are_exact_and_persistent() {
    let runtime = TestDir::new("name-runtime");
    let cwd = TestDir::new("name-cwd");
    let commands = TestDir::new("name-command");
    symlink("/bin/sleep", commands.0.join("codex")).unwrap();
    spawn_named(
        &commands.0.join("codex"),
        &cwd.0,
        "DO_NOT_CAPTURE_NAME_SENTINEL",
    );
    let daemon = Daemon::start(&runtime.0);
    let initial = wait_for_matching_agent(&daemon.socket, &cwd.0);
    let agent = initial
        .agents
        .iter()
        .find(|agent| {
            agent.cwd.state == CwdState::Known
                && agent.cwd.value.as_deref() == Some(cwd.0.to_string_lossy().as_ref())
        })
        .unwrap();
    let id = agent.id;
    let pid = id.pid;
    assert!(agent.started_at_unix_ms.is_some());
    assert_eq!(agent.name, None);
    let initial_revision = initial.revision;

    let set = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args(["name", "--pid", &pid.to_string(), "Agentd spec"])
        .env("XDG_RUNTIME_DIR", &runtime.0)
        .output()
        .unwrap();
    assert_silent_success(set);
    let named: Snapshot = serde_json::from_slice(&request(
        &daemon.socket,
        b"{\"version\":1,\"op\":\"snapshot\"}\n",
    ))
    .unwrap();
    assert_eq!(named.revision, initial_revision + 1);
    assert_eq!(
        named
            .agents
            .iter()
            .find(|agent| agent.id == id)
            .unwrap()
            .name
            .as_deref(),
        Some("Agentd spec")
    );

    let identical = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args(["name", "--pid", &pid.to_string(), "Agentd spec"])
        .env("XDG_RUNTIME_DIR", &runtime.0)
        .output()
        .unwrap();
    assert_silent_success(identical);
    let unchanged: Snapshot = serde_json::from_slice(&request(
        &daemon.socket,
        b"{\"version\":1,\"op\":\"snapshot\"}\n",
    ))
    .unwrap();
    assert_eq!(unchanged.revision, named.revision);

    let registry = runtime.0.join("state/agentd/names.json");
    let registry_bytes = fs::read(&registry).unwrap();
    assert_eq!(
        fs::symlink_metadata(&registry)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(!String::from_utf8_lossy(&registry_bytes).contains("DO_NOT_CAPTURE_NAME_SENTINEL"));
    daemon.stop();

    let daemon = Daemon::start(&runtime.0);
    let restarted = wait_for_matching_agent(&daemon.socket, &cwd.0);
    let restarted_agent = restarted
        .agents
        .iter()
        .find(|agent| agent.id == id)
        .unwrap();
    assert_eq!(restarted.revision, 1);
    assert_eq!(restarted_agent.name.as_deref(), Some("Agentd spec"));
    assert_eq!(restarted_agent.activity.state, ActivityState::Unknown);

    let wrong_identity = format!(
        "{{\"version\":1,\"op\":\"name\",\"agent\":{{\"pid\":{pid},\"startTimeTicks\":{}}},\"name\":\"wrong\"}}\n",
        id.start_time_ticks + 1
    );
    assert_error(
        &request(&daemon.socket, wrong_identity.as_bytes()),
        "unknown_agent",
    );
    let clear = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args(["name", "--pid", &pid.to_string(), "--clear"])
        .env("XDG_RUNTIME_DIR", &runtime.0)
        .output()
        .unwrap();
    assert_silent_success(clear);
    let clear_again = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args(["name", "--pid", &pid.to_string(), "--clear"])
        .env("XDG_RUNTIME_DIR", &runtime.0)
        .output()
        .unwrap();
    assert_silent_success(clear_again);
    let cleared: Snapshot = serde_json::from_slice(&request(
        &daemon.socket,
        b"{\"version\":1,\"op\":\"snapshot\"}\n",
    ))
    .unwrap();
    assert_eq!(
        cleared
            .agents
            .iter()
            .find(|agent| agent.id == id)
            .unwrap()
            .name,
        None
    );
    assert!(!String::from_utf8_lossy(&fs::read(&registry).unwrap()).contains("Agentd spec"));

    daemon.stop();
    let result = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    assert_eq!(result, 0);
}

#[test]
fn version_reports_the_package_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        output.stdout,
        format!("agentd {}\n", env!("CARGO_PKG_VERSION")).into_bytes()
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn integration_cli_gates_codex_and_names_restart_only_activation() {
    let pkg_version = env!("CARGO_PKG_VERSION");
    let root = TestDir::new("integrate-cli");
    let commands = root.0.join("commands");
    let claude = root.0.join("claude");
    let codex = root.0.join("codex");
    fs::create_dir(&commands).unwrap();
    fs::create_dir(&claude).unwrap();
    fs::create_dir(&codex).unwrap();
    let fake_codex = commands.join("codex");
    fs::write(
        &fake_codex,
        b"#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'codex-cli 0.150.0'; exit 0; fi\nif [ \"$1 $2\" = \"features list\" ]; then echo 'hooks stable true'; exit 0; fi\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&fake_codex, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        commands.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let trust = codex.join("config.toml");
    let trust_bytes = b"[hooks.state]\ntrusted_hash = \"codex-owned\"\n";
    fs::write(&trust, trust_bytes).unwrap();

    let claude_install = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args(["integrate", "install", "claude"])
        .env("CLAUDE_CONFIG_DIR", &claude)
        .output()
        .unwrap();
    assert!(claude_install.status.success(), "{claude_install:?}");
    let claude_output = String::from_utf8(claude_install.stdout).unwrap();
    assert_eq!(
        claude_output,
        format!(
            "agentd integrate: agentd_version={pkg_version} harness=claude action=install result=changed target={} not_removed=[] existing_process=kept_by_procfs activity=unchanged next_activity=accepted_mapped_hook_event activation=restart_only resume=\"claude --continue|claude --resume\"\n",
            claude.join("settings.json").display()
        )
    );
    let claude_uninstall = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args(["integrate", "uninstall", "claude"])
        .env("CLAUDE_CONFIG_DIR", &claude)
        .output()
        .unwrap();
    assert!(claude_uninstall.status.success(), "{claude_uninstall:?}");
    assert_eq!(
        String::from_utf8(claude_uninstall.stdout).unwrap(),
        format!(
            "agentd integrate: agentd_version={pkg_version} harness=claude action=uninstall result=changed target={} not_removed=[]\n",
            claude.join("settings.json").display()
        )
    );

    let codex_install = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args(["integrate", "install", "codex"])
        .env("CODEX_HOME", &codex)
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(codex_install.status.success(), "{codex_install:?}");
    let codex_output = String::from_utf8(codex_install.stdout).unwrap();
    assert_eq!(
        codex_output,
        format!(
            "agentd integrate: agentd_version={pkg_version} harness=codex action=install result=changed target={} not_removed=[] existing_process=kept_by_procfs activity=unchanged next_activity=accepted_mapped_hook_event activation=restart_only resume=\"codex resume\" trust=next_interactive_startup_review warning=unverified_codex_version version=codex-cli 0.150.0\n",
            codex.join("hooks.json").display()
        )
    );
    assert_eq!(fs::read(&trust).unwrap(), trust_bytes);

    let installed = fs::read(codex.join("hooks.json")).unwrap();
    let second = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args(["integrate", "install", "codex"])
        .env("CODEX_HOME", &codex)
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(second.status.success(), "{second:?}");
    assert_eq!(
        String::from_utf8(second.stdout).unwrap(),
        format!(
            "agentd integrate: agentd_version={pkg_version} harness=codex action=install result=unchanged target={} not_removed=[] existing_process=kept_by_procfs activity=unchanged next_activity=accepted_mapped_hook_event activation=restart_only resume=\"codex resume\" trust=next_interactive_startup_review warning=unverified_codex_version version=codex-cli 0.150.0\n",
            codex.join("hooks.json").display()
        )
    );
    assert_eq!(fs::read(codex.join("hooks.json")).unwrap(), installed);

    let uninstall = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args(["integrate", "uninstall", "codex"])
        .env("CODEX_HOME", &codex)
        .env("PATH", "/nonexistent")
        .output()
        .unwrap();
    assert!(uninstall.status.success(), "{uninstall:?}");
    assert_eq!(
        String::from_utf8(uninstall.stdout).unwrap(),
        format!(
            "agentd integrate: agentd_version={pkg_version} harness=codex action=uninstall result=changed target={} not_removed=[]\n",
            codex.join("hooks.json").display()
        )
    );
    assert_eq!(fs::read(&trust).unwrap(), trust_bytes);
    let hooks: Value =
        serde_json::from_slice(&fs::read(codex.join("hooks.json")).unwrap()).unwrap();
    assert!(hooks["hooks"].as_object().unwrap().is_empty());

    fs::write(
        &fake_codex,
        b"#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'codex-cli 0.149.1'; exit 0; fi\nif [ \"$1 $2\" = \"features list\" ]; then echo 'hooks stable false'; exit 0; fi\nexit 1\n",
    )
    .unwrap();
    let before_rejected_install = fs::read(codex.join("hooks.json")).unwrap();
    let rejected = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args(["integrate", "install", "codex"])
        .env("CODEX_HOME", &codex)
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
    assert_eq!(
        String::from_utf8(rejected.stderr).unwrap(),
        "agentd integrate: unsupported_codex_hooks\n"
    );
    assert_eq!(
        fs::read(codex.join("hooks.json")).unwrap(),
        before_rejected_install
    );
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

fn wait_for_matching_agent(path: &Path, cwd: &Path) -> Snapshot {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let snapshot: Snapshot =
            serde_json::from_slice(&request(path, b"{\"version\":1,\"op\":\"snapshot\"}\n"))
                .unwrap();
        if !matching_agents(&snapshot, cwd).is_empty() {
            return snapshot;
        }
        assert!(Instant::now() < deadline, "agent did not enter roster");
        thread::sleep(Duration::from_millis(20));
    }
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
