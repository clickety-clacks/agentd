use crate::model::{ActivityState, Snapshot};
use crate::procfs::parse_stat;
use crate::protocol::human_snapshot;
use crate::server;
use serde::Deserialize;
use serde_json::Value;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

pub fn run(arguments: Vec<OsString>) -> Result<(), String> {
    let arguments: Vec<String> = arguments
        .into_iter()
        .map(|argument| {
            argument
                .into_string()
                .map_err(|_| "agentd usage failed: arguments must be UTF-8".to_owned())
        })
        .collect::<Result<_, _>>()?;
    match arguments.as_slice() {
        [command] if command == "daemon" => server::run_daemon()
            .map_err(|cause| format!("agentd daemon failed: {cause}")),
        [command] if command == "list" => list(false),
        [command, flag] if command == "list" && flag == "--json" => list(true),
        [command] if command == "watch" => watch(false),
        [command, flag] if command == "watch" && flag == "--json" => watch(true),
        [command, pid_flag, pid, state_flag, state]
            if command == "activity" && pid_flag == "--pid" && state_flag == "--state" =>
        {
            activity(pid, state)
        }
        _ => Err(
            "agentd usage failed: expected daemon | list [--json] | watch [--json] | activity --pid <positive-integer> --state active|idle"
                .to_owned(),
        ),
    }
}

fn list(json: bool) -> Result<(), String> {
    let mut stream = connect("list")?;
    stream
        .write_all(b"{\"version\":1,\"op\":\"snapshot\"}\n")
        .map_err(|error| format!("agentd list failed: request write: {error}"))?;
    let frame = read_frame(&mut stream, "list")?;
    let snapshot = parse_snapshot_or_error(&frame, "list")?;
    if json {
        std::io::stdout()
            .write_all(&frame)
            .map_err(|error| format!("agentd list failed: stdout write: {error}"))?;
    } else {
        print!("{}", human_snapshot(&snapshot));
    }
    Ok(())
}

fn watch(json: bool) -> Result<(), String> {
    let mut stream = connect("watch")?;
    stream
        .write_all(b"{\"version\":1,\"op\":\"subscribe\"}\n")
        .map_err(|error| format!("agentd watch failed: request write: {error}"))?;
    let mut reader = BufReader::new(stream);
    loop {
        let mut frame = Vec::new();
        let count = reader
            .read_until(b'\n', &mut frame)
            .map_err(|error| format!("agentd watch failed: response read: {error}"))?;
        if count == 0 {
            return Err("agentd watch failed: daemon closed subscription".to_owned());
        }
        let snapshot = parse_snapshot_or_error(&frame, "watch")?;
        if json {
            std::io::stdout()
                .write_all(&frame)
                .map_err(|error| format!("agentd watch failed: stdout write: {error}"))?;
            std::io::stdout()
                .flush()
                .map_err(|error| format!("agentd watch failed: stdout flush: {error}"))?;
        } else {
            print!("{}", human_snapshot(&snapshot));
            std::io::stdout()
                .flush()
                .map_err(|error| format!("agentd watch failed: stdout flush: {error}"))?;
        }
    }
}

fn activity(pid: &str, state: &str) -> Result<(), String> {
    let pid: u32 = pid
        .parse()
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or_else(|| "agentd activity failed: pid must be a positive integer".to_owned())?;
    let state = match state {
        "active" => ActivityState::Active,
        "idle" => ActivityState::Idle,
        _ => {
            return Err("agentd activity failed: state must be active or idle".to_owned());
        }
    };
    let stat_path = format!("/proc/{pid}/stat");
    let stat = fs::read_to_string(&stat_path)
        .map_err(|error| format!("agentd activity failed: read {stat_path}: {error}"))?;
    let start_time_ticks = parse_stat(&stat, pid)
        .map_err(|error| format!("agentd activity failed: parse {stat_path}: {error}"))?
        .start_time_ticks;
    let state_name = match state {
        ActivityState::Active => "active",
        ActivityState::Idle => "idle",
        ActivityState::Unknown => unreachable!(),
    };
    let request = format!(
        "{{\"version\":1,\"op\":\"activity\",\"agent\":{{\"pid\":{pid},\"startTimeTicks\":{start_time_ticks}}},\"state\":\"{state_name}\"}}\n"
    );
    let mut stream = connect("activity")?;
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("agentd activity failed: request write: {error}"))?;
    let frame = read_frame(&mut stream, "activity")?;
    parse_ack_or_error(&frame)?;
    Ok(())
}

fn connect(operation: &str) -> Result<UnixStream, String> {
    let path =
        server::socket_path().map_err(|cause| format!("agentd {operation} failed: {cause}"))?;
    UnixStream::connect(&path).map_err(|error| socket_error(operation, &path, &error))
}

fn socket_error(operation: &str, path: &Path, error: &std::io::Error) -> String {
    format!(
        "agentd {operation} failed: socket {} unavailable: {error}; run systemctl --user status agentd.service",
        path.display()
    )
}

fn read_frame(stream: &mut UnixStream, operation: &str) -> Result<Vec<u8>, String> {
    let mut reader = BufReader::new(stream);
    let mut frame = Vec::new();
    let count = reader
        .read_until(b'\n', &mut frame)
        .map_err(|error| format!("agentd {operation} failed: response read: {error}"))?;
    if count == 0 || frame.last() != Some(&b'\n') {
        return Err(format!(
            "agentd {operation} failed: daemon returned an incomplete frame"
        ));
    }
    Ok(frame)
}

fn parse_snapshot_or_error(frame: &[u8], operation: &str) -> Result<Snapshot, String> {
    let value: Value = serde_json::from_slice(frame)
        .map_err(|error| format!("agentd {operation} failed: invalid response JSON: {error}"))?;
    if value.get("type").and_then(Value::as_str) == Some("error") {
        let error: ReceivedError = serde_json::from_value(value)
            .map_err(|error| format!("agentd {operation} failed: invalid error frame: {error}"))?;
        return Err(format!(
            "agentd {operation} failed: {}: {}",
            error.code, error.message
        ));
    }
    serde_json::from_value(value)
        .map_err(|error| format!("agentd {operation} failed: invalid snapshot frame: {error}"))
}

fn parse_ack_or_error(frame: &[u8]) -> Result<(), String> {
    let value: Value = serde_json::from_slice(frame)
        .map_err(|error| format!("agentd activity failed: invalid response JSON: {error}"))?;
    match value.get("type").and_then(Value::as_str) {
        Some("ack") => {
            let object = value
                .as_object()
                .ok_or_else(|| "agentd activity failed: invalid acknowledgement".to_owned())?;
            if object.len() != 3
                || !object.contains_key("instanceId")
                || !object.contains_key("revision")
            {
                return Err("agentd activity failed: invalid acknowledgement fields".to_owned());
            }
            if object.get("instanceId").and_then(Value::as_str).is_none()
                || object.get("revision").and_then(Value::as_u64).is_none()
            {
                return Err("agentd activity failed: invalid acknowledgement values".to_owned());
            }
            Ok(())
        }
        Some("error") => {
            let error: ReceivedError = serde_json::from_value(value)
                .map_err(|error| format!("agentd activity failed: invalid error frame: {error}"))?;
            Err(format!(
                "agentd activity failed: {}: {}",
                error.code, error.message
            ))
        }
        _ => Err("agentd activity failed: unexpected response type".to_owned()),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceivedError {
    #[serde(rename = "type")]
    _frame_type: String,
    code: String,
    message: String,
}
