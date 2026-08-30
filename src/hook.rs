use crate::model::{ActivityState, AgentId, Harness};
use crate::names::validate_display_name;
use crate::procfs::{HookResolutionError, ProcfsScanner};
use crate::server;
use serde_json::Value;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const TOTAL_DEADLINE: Duration = Duration::from_millis(500);
const MAX_RESPONSE_BYTES: usize = 65_536;
const CLAUDE_MAPPINGS: &[(&str, ActivityState)] = &[
    ("UserPromptSubmit", ActivityState::Active),
    ("PreToolUse", ActivityState::Active),
    ("Stop", ActivityState::Idle),
    ("Notification", ActivityState::NeedsAttention),
];
const CODEX_MAPPINGS: &[(&str, ActivityState)] = &[
    ("UserPromptSubmit", ActivityState::Active),
    ("PreToolUse", ActivityState::Active),
    ("PermissionRequest", ActivityState::NeedsAttention),
    ("Stop", ActivityState::Idle),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HookError {
    RuntimeDirectoryUnavailable,
    SocketUnavailable,
    ProtocolError,
    AncestryUnresolved,
    ProcessIdentityChanged,
    UnknownAgent,
    NameStoreUnavailable,
    DeadlineExceeded,
}

impl HookError {
    fn as_str(self) -> &'static str {
        match self {
            Self::RuntimeDirectoryUnavailable => "runtime_directory_unavailable",
            Self::SocketUnavailable => "socket_unavailable",
            Self::ProtocolError => "protocol_error",
            Self::AncestryUnresolved => "ancestry_unresolved",
            Self::ProcessIdentityChanged => "process_identity_changed",
            Self::UnknownAgent => "unknown_agent",
            Self::NameStoreUnavailable => "name_store_unavailable",
            Self::DeadlineExceeded => "deadline_exceeded",
        }
    }
}

pub fn run_name(name: &str) -> Result<(), String> {
    if !validate_display_name(name) {
        return Err("agentd name: invalid_name".to_owned());
    }
    if let Err(error) = run_named(name.to_owned()) {
        eprintln!("agentd name: {}", error.as_str());
    }
    Ok(())
}

pub fn run(harness: &str, event: &str) -> Result<(), String> {
    let (harness, state) =
        mapping(harness, event).ok_or_else(|| "agentd hook: invalid_hook_event".to_owned())?;
    if let Err(error) = run_mapped(harness, state) {
        eprintln!("agentd hook: {}", error.as_str());
    }
    Ok(())
}

fn mapping(harness: &str, event: &str) -> Option<(Harness, ActivityState)> {
    let harness = match harness {
        "claude" => Harness::Claude,
        "codex" => Harness::Codex,
        _ => return None,
    };
    mappings_for(harness)
        .iter()
        .find(|(candidate, _)| *candidate == event)
        .map(|(_, state)| (harness, *state))
}

pub(crate) fn mappings_for(harness: Harness) -> &'static [(&'static str, ActivityState)] {
    match harness {
        Harness::Claude => CLAUDE_MAPPINGS,
        Harness::Codex => CODEX_MAPPINGS,
    }
}

fn run_mapped(harness: Harness, state: ActivityState) -> Result<(), HookError> {
    let deadline = Instant::now() + TOTAL_DEADLINE;
    drain_stdin(deadline)?;
    check_deadline(deadline)?;

    let parent_pid = unsafe { libc::getppid() };
    let parent_pid = u32::try_from(parent_pid)
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or(HookError::AncestryUnresolved)?;
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = resolve_and_submit(parent_pid, harness, state, deadline);
        let _ = sender.send(result);
    });
    receiver
        .recv_timeout(remaining(deadline)?)
        .map_err(|_| HookError::DeadlineExceeded)?
}

fn run_named(name: String) -> Result<(), HookError> {
    let deadline = Instant::now() + TOTAL_DEADLINE;
    drain_stdin(deadline)?;
    check_deadline(deadline)?;
    let parent_pid = unsafe { libc::getppid() };
    let parent_pid = u32::try_from(parent_pid)
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or(HookError::AncestryUnresolved)?;
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = resolve_and_submit_name(parent_pid, name, deadline);
        let _ = sender.send(result);
    });
    receiver
        .recv_timeout(remaining(deadline)?)
        .map_err(|_| HookError::DeadlineExceeded)?
}

fn resolve_and_submit(
    parent_pid: u32,
    harness: Harness,
    state: ActivityState,
    deadline: Instant,
) -> Result<(), HookError> {
    let id = ProcfsScanner::system()
        .resolve_hook_root(parent_pid, harness)
        .map_err(|error| match error {
            HookResolutionError::AncestryUnresolved => HookError::AncestryUnresolved,
            HookResolutionError::ProcessIdentityChanged => HookError::ProcessIdentityChanged,
        })?;
    check_deadline(deadline)?;

    submit_activity(id, state, deadline)
}

fn resolve_and_submit_name(
    parent_pid: u32,
    name: String,
    deadline: Instant,
) -> Result<(), HookError> {
    let id = ProcfsScanner::system()
        .resolve_hook_root(parent_pid, Harness::Claude)
        .map_err(|error| match error {
            HookResolutionError::AncestryUnresolved => HookError::AncestryUnresolved,
            HookResolutionError::ProcessIdentityChanged => HookError::ProcessIdentityChanged,
        })?;
    check_deadline(deadline)?;
    submit_name(id, &name, deadline)
}

fn drain_stdin(deadline: Instant) -> Result<(), HookError> {
    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let mut buffer = [0_u8; 8192];
    loop {
        let remaining = remaining(deadline)?;
        let timeout_ms = remaining.as_millis().clamp(1, i32::MAX as u128) as i32;
        let mut descriptor = libc::pollfd {
            fd: stdin.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
        if result == 0 {
            return Err(HookError::DeadlineExceeded);
        }
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(HookError::ProtocolError);
        }
        match stdin.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(HookError::ProtocolError),
        }
    }
}

fn submit_activity(id: AgentId, state: ActivityState, deadline: Instant) -> Result<(), HookError> {
    let path = server::socket_path().map_err(|_| HookError::RuntimeDirectoryUnavailable)?;
    let mut stream = UnixStream::connect(path).map_err(|_| HookError::SocketUnavailable)?;
    set_timeouts(&stream, deadline)?;
    let request = format!(
        "{{\"version\":1,\"op\":\"activity\",\"agent\":{{\"pid\":{},\"startTimeTicks\":{}}},\"state\":\"{}\"}}\n",
        id.pid,
        id.start_time_ticks,
        state.as_str()
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|error| io_error(error, deadline))?;
    let frame = read_frame(&mut stream, deadline)?;
    parse_ack(&frame)
}

fn submit_name(id: AgentId, name: &str, deadline: Instant) -> Result<(), HookError> {
    let path = server::socket_path().map_err(|_| HookError::RuntimeDirectoryUnavailable)?;
    let mut stream = UnixStream::connect(path).map_err(|_| HookError::SocketUnavailable)?;
    set_timeouts(&stream, deadline)?;
    let request = serde_json::json!({
        "version": 1,
        "op": "name",
        "agent": {"pid": id.pid, "startTimeTicks": id.start_time_ticks},
        "name": name,
    });
    let mut request = serde_json::to_vec(&request).map_err(|_| HookError::ProtocolError)?;
    request.push(b'\n');
    stream
        .write_all(&request)
        .map_err(|error| io_error(error, deadline))?;
    let frame = read_frame(&mut stream, deadline)?;
    parse_ack(&frame)
}

fn read_frame(stream: &mut UnixStream, deadline: Instant) -> Result<Vec<u8>, HookError> {
    let mut frame = Vec::with_capacity(256);
    let mut chunk = [0_u8; 1024];
    loop {
        set_timeouts(stream, deadline)?;
        let count = stream
            .read(&mut chunk)
            .map_err(|error| io_error(error, deadline))?;
        if count == 0 {
            return Err(HookError::ProtocolError);
        }
        for byte in &chunk[..count] {
            if *byte == b'\n' {
                return Ok(frame);
            }
            if frame.len() == MAX_RESPONSE_BYTES {
                return Err(HookError::ProtocolError);
            }
            frame.push(*byte);
        }
    }
}

fn parse_ack(frame: &[u8]) -> Result<(), HookError> {
    let value: Value = serde_json::from_slice(frame).map_err(|_| HookError::ProtocolError)?;
    let object = value.as_object().ok_or(HookError::ProtocolError)?;
    match object.get("type").and_then(Value::as_str) {
        Some("ack")
            if object.len() == 3
                && object.get("instanceId").and_then(Value::as_str).is_some()
                && object.get("revision").and_then(Value::as_u64).is_some() =>
        {
            Ok(())
        }
        Some("error") if object.get("code").and_then(Value::as_str) == Some("unknown_agent") => {
            Err(HookError::UnknownAgent)
        }
        Some("error")
            if object.get("code").and_then(Value::as_str) == Some("name_store_unavailable") =>
        {
            Err(HookError::NameStoreUnavailable)
        }
        _ => Err(HookError::ProtocolError),
    }
}

fn set_timeouts(stream: &UnixStream, deadline: Instant) -> Result<(), HookError> {
    let remaining = remaining(deadline)?;
    stream
        .set_read_timeout(Some(remaining))
        .map_err(|_| HookError::ProtocolError)?;
    stream
        .set_write_timeout(Some(remaining))
        .map_err(|_| HookError::ProtocolError)
}

fn remaining(deadline: Instant) -> Result<Duration, HookError> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or(HookError::DeadlineExceeded)
}

fn check_deadline(deadline: Instant) -> Result<(), HookError> {
    remaining(deadline).map(|_| ())
}

fn io_error(error: io::Error, deadline: Instant) -> HookError {
    if matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ) || Instant::now() >= deadline
    {
        HookError::DeadlineExceeded
    } else {
        HookError::ProtocolError
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mappings_are_closed_and_exact() {
        assert_eq!(
            mapping("claude", "Notification"),
            Some((Harness::Claude, ActivityState::NeedsAttention))
        );
        assert_eq!(
            mapping("codex", "PermissionRequest"),
            Some((Harness::Codex, ActivityState::NeedsAttention))
        );
        assert_eq!(mapping("codex", "Notification"), None);
        assert_eq!(mapping("claude", "PermissionRequest"), None);
    }
}
