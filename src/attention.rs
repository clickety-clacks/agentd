use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashSet, VecDeque};
use std::env;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const RETRY_START: Duration = Duration::from_millis(250);
const RETRY_MAX: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const EVENT_POLL: Duration = Duration::from_millis(100);

pub fn run() -> Result<(), String> {
    let machine = read_machine_name()?;
    let runtime_dir = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| "XDG_RUNTIME_DIR is required".to_owned())?;
    if !runtime_dir.is_absolute() {
        return Err("XDG_RUNTIME_DIR must be an absolute path".to_owned());
    }
    supervise(
        machine,
        runtime_dir.join("attention").join("attention.sock"),
    )
}

fn supervise(machine: String, socket_path: PathBuf) -> Result<(), String> {
    let mut backoff = Backoff::default();
    loop {
        if let Err(error) = run_attempt(&machine, &socket_path, &mut backoff) {
            eprintln!("agentd-attention: {error}; retrying");
            thread::sleep(backoff.next());
        }
    }
}

fn run_attempt(machine: &str, socket_path: &Path, backoff: &mut Backoff) -> Result<(), String> {
    let (event_tx, event_rx) = mpsc::channel();
    let mut watch = AgentdWatch::start(event_tx.clone())?;
    let mut latest = Some(wait_for_complete_snapshot(&event_rx, &mut watch)?);

    loop {
        if let Some(status) = watch
            .child
            .try_wait()
            .map_err(|error| format!("checking Agentd watch: {error}"))?
        {
            return Err(format!("Agentd watch exited with {status}"));
        }

        if let Some(snapshot) = latest.as_ref() {
            match UnixStream::connect(socket_path) {
                Ok(stream) => {
                    let mut attention = AttentionClient::new(stream, event_tx, event_rx)?;
                    let mut state = initial_sync(machine, snapshot, &mut attention)?;
                    backoff.reset();
                    return run_connected(machine, &mut watch, &mut attention, &mut state);
                }
                Err(error) => {
                    eprintln!(
                        "agentd-attention: local attention socket unavailable at {}: {error}",
                        socket_path.display()
                    );
                    let deadline = Instant::now() + backoff.next();
                    loop {
                        let remaining = deadline.saturating_duration_since(Instant::now());
                        if remaining.is_zero() {
                            break;
                        }
                        match event_rx.recv_timeout(remaining) {
                            Ok(BridgeEvent::AgentdSnapshot(Ok(snapshot))) => {
                                latest = snapshot.is_complete().then_some(snapshot);
                            }
                            Ok(BridgeEvent::AgentdSnapshot(Err(error))) => {
                                eprintln!("agentd-attention: ignoring Agentd frame: {error}");
                            }
                            Ok(BridgeEvent::AgentdEnded) => {
                                return Err("Agentd watch disconnected".to_owned());
                            }
                            Ok(BridgeEvent::AttentionFrame(_))
                            | Ok(BridgeEvent::AttentionEnded) => {
                                return Err(
                                    "unexpected attention event before connecting".to_owned()
                                );
                            }
                            Err(RecvTimeoutError::Timeout) => break,
                            Err(RecvTimeoutError::Disconnected) => {
                                return Err("local event readers stopped".to_owned());
                            }
                        }
                    }
                    continue;
                }
            }
        }

        match event_rx.recv_timeout(backoff.next()) {
            Ok(BridgeEvent::AgentdSnapshot(Ok(snapshot))) => {
                latest = snapshot.is_complete().then_some(snapshot);
            }
            Ok(BridgeEvent::AgentdSnapshot(Err(error))) => {
                eprintln!("agentd-attention: ignoring Agentd frame: {error}");
            }
            Ok(BridgeEvent::AgentdEnded) => return Err("Agentd watch disconnected".to_owned()),
            Ok(BridgeEvent::AttentionFrame(_)) | Ok(BridgeEvent::AttentionEnded) => {
                return Err("unexpected attention event before connecting".to_owned());
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err("local event readers stopped".to_owned());
            }
        }
    }
}

fn wait_for_complete_snapshot(
    events: &Receiver<BridgeEvent>,
    watch: &mut AgentdWatch,
) -> Result<PublishedSnapshot, String> {
    let mut backoff = Backoff::default();
    loop {
        if let Some(status) = watch
            .child
            .try_wait()
            .map_err(|error| format!("checking Agentd watch: {error}"))?
        {
            return Err(format!("Agentd watch exited with {status}"));
        }
        match events.recv_timeout(backoff.next()) {
            Ok(BridgeEvent::AgentdSnapshot(Ok(snapshot))) if snapshot.is_complete() => {
                return Ok(snapshot);
            }
            Ok(BridgeEvent::AgentdSnapshot(Ok(_))) => {
                eprintln!("agentd-attention: waiting for a complete Agentd scan");
            }
            Ok(BridgeEvent::AgentdSnapshot(Err(error))) => {
                eprintln!("agentd-attention: ignoring Agentd frame: {error}");
            }
            Ok(BridgeEvent::AgentdEnded) => return Err("Agentd watch disconnected".to_owned()),
            Ok(BridgeEvent::AttentionFrame(_)) | Ok(BridgeEvent::AttentionEnded) => {
                return Err("unexpected attention event before connecting".to_owned());
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err("local event readers stopped".to_owned());
            }
        }
    }
}

fn run_connected(
    machine: &str,
    watch: &mut AgentdWatch,
    attention: &mut AttentionClient,
    state: &mut BridgeState,
) -> Result<(), String> {
    loop {
        if let Some(event) = attention.pending.pop_front() {
            process_connected_event(machine, attention, state, event)?;
            continue;
        }
        if let Some(status) = watch
            .child
            .try_wait()
            .map_err(|error| format!("checking Agentd watch: {error}"))?
        {
            return Err(format!("Agentd watch exited with {status}"));
        }
        match attention.events.recv_timeout(EVENT_POLL) {
            Ok(event) => process_connected_event(machine, attention, state, event)?,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err("local event readers stopped".to_owned());
            }
        }
    }
}

fn process_connected_event(
    machine: &str,
    attention: &mut AttentionClient,
    state: &mut BridgeState,
    event: BridgeEvent,
) -> Result<(), String> {
    match event {
        BridgeEvent::AgentdSnapshot(Ok(snapshot)) => {
            if snapshot.instance_id != state.instance_id {
                if snapshot.is_complete() {
                    *state = initial_sync(machine, &snapshot, attention)?;
                }
                return Ok(());
            }
            state.reconcile(&snapshot, attention, now_unix_ms()?)
        }
        BridgeEvent::AgentdSnapshot(Err(error)) => {
            eprintln!("agentd-attention: ignoring Agentd frame: {error}");
            Ok(())
        }
        BridgeEvent::AgentdEnded => Err("Agentd watch disconnected".to_owned()),
        BridgeEvent::AttentionFrame(frame) => state.observe_attention_frame(&frame),
        BridgeEvent::AttentionEnded => Err("attention daemon disconnected".to_owned()),
    }
}

fn initial_sync(
    machine: &str,
    snapshot: &PublishedSnapshot,
    attention: &mut impl AttentionSink,
) -> Result<BridgeState, String> {
    if !snapshot.is_complete() {
        return Err("refusing to synchronize before a complete Agentd snapshot".to_owned());
    }
    let mut state = BridgeState::new(machine, &snapshot.instance_id);
    state.post_current_claims(snapshot, attention)?;
    let messages = attention.subscribe_empty_snapshot()?;
    state.replace_open_messages(messages);
    state.reconcile(snapshot, attention, now_unix_ms()?)?;
    Ok(state)
}

fn read_machine_name() -> Result<String, String> {
    let output = Command::new("uname")
        .arg("-n")
        .output()
        .map_err(|error| format!("reading the local node name with uname -n: {error}"))?;
    if !output.status.success() {
        return Err(format!("uname -n exited with {}", output.status));
    }
    let bytes = output
        .stdout
        .strip_suffix(b"\n")
        .unwrap_or(output.stdout.as_slice());
    let raw = std::str::from_utf8(bytes)
        .map_err(|_| "uname -n returned a non-UTF-8 node name".to_owned())?;
    normalize_machine(raw)
}

fn normalize_machine(raw: &str) -> Result<String, String> {
    let lowered = raw.to_lowercase();
    let machine = lowered.strip_suffix('.').unwrap_or(&lowered);
    if machine.is_empty()
        || machine.chars().any(|character| {
            matches!(character, '/' | ':') || character.is_whitespace() || character.is_control()
        })
    {
        return Err(format!(
            "invalid node name for an Agentd sender id: {raw:?}; no messages will be posted"
        ));
    }
    Ok(machine.to_owned())
}

fn now_unix_ms() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system time is before the Unix epoch: {error}"))?
        .as_millis()
        .try_into()
        .map_err(|_| "system time does not fit in Unix milliseconds".to_owned())
}

fn parse_agentd_snapshot(frame: &[u8]) -> Result<PublishedSnapshot, String> {
    let snapshot: PublishedSnapshot = serde_json::from_slice(frame)
        .map_err(|error| format!("invalid published snapshot JSON: {error}"))?;
    if snapshot.frame_type != "snapshot" {
        return Err("Agentd frame type is not snapshot".to_owned());
    }
    if snapshot.schema != "agentd.snapshot.v1" {
        return Err("Agentd schema is not agentd.snapshot.v1".to_owned());
    }
    if snapshot.instance_id.is_empty() {
        return Err("Agentd instanceId is empty".to_owned());
    }
    Ok(snapshot)
}

fn build_message(
    machine: &str,
    instance_id: &str,
    agent: &PublishedAgent,
    observed_at_unix_ms: u64,
) -> Result<BridgeMessage, String> {
    let sender_id = format!(
        "agentd://{machine}/{instance_id}/{}:{}",
        agent.id.pid, agent.id.start_time_ticks
    );
    if !valid_sender_id(&sender_id) {
        return Err("Agentd identity cannot form a valid sender id".to_owned());
    }
    let label = agent
        .name
        .as_deref()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            agent
                .tmux
                .as_ref()
                .map(|tmux| tmux.session.as_str())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or(&agent.harness);
    let open = agent
        .tmux
        .as_ref()
        .filter(|tmux| !tmux.session.is_empty())
        .map(|tmux| OpenCommand {
            argv: vec![
                "tmux".to_owned(),
                "attach-session".to_owned(),
                "-t".to_owned(),
                format!("={}", tmux.session),
            ],
            terminal: true,
        });
    Ok(BridgeMessage {
        v: 1,
        frame_type: "message",
        sender_id,
        message_id: format!("needs_attention@{observed_at_unix_ms}"),
        created_at: observed_at_unix_ms,
        title: format!("{label} on {machine}"),
        open,
    })
}

fn valid_sender_id(sender_id: &str) -> bool {
    sender_id
        .strip_prefix("agentd:")
        .is_some_and(|rest| !rest.is_empty())
        && !sender_id
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublishedSnapshot {
    #[serde(rename = "type")]
    frame_type: String,
    schema: String,
    instance_id: String,
    revision: u64,
    scan: PublishedScan,
    agents: Vec<PublishedAgent>,
}

impl PublishedSnapshot {
    fn is_complete(&self) -> bool {
        self.scan.state == "complete"
    }
}

#[derive(Clone, Debug, Deserialize)]
struct PublishedScan {
    state: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublishedAgent {
    id: PublishedAgentId,
    harness: String,
    activity: PublishedActivity,
    name: Option<String>,
    tmux: Option<PublishedTmux>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublishedAgentId {
    pid: u32,
    start_time_ticks: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublishedActivity {
    state: String,
    observed_at_unix_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
struct PublishedTmux {
    session: String,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct GlobalId {
    sender_id: String,
    message_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct BridgeMessage {
    v: u8,
    #[serde(rename = "type")]
    frame_type: &'static str,
    sender_id: String,
    message_id: String,
    created_at: u64,
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    open: Option<OpenCommand>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct OpenCommand {
    argv: Vec<String>,
    terminal: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PostResult {
    Accepted,
    Duplicate,
}

trait AttentionSink {
    fn post(&mut self, message: &BridgeMessage) -> Result<PostResult, String>;
    fn lifecycle(&mut self, id: &GlobalId, state: &str, at: u64) -> Result<(), String>;
    fn subscribe_empty_snapshot(&mut self) -> Result<Vec<GlobalId>, String>;
}

struct BridgeState {
    machine: String,
    instance_id: String,
    local_prefix: String,
    last_revision: Option<u64>,
    attempted: HashSet<GlobalId>,
    open: BTreeSet<GlobalId>,
}

impl BridgeState {
    fn new(machine: &str, instance_id: &str) -> Self {
        Self {
            machine: machine.to_owned(),
            instance_id: instance_id.to_owned(),
            local_prefix: format!("agentd://{machine}/"),
            last_revision: None,
            attempted: HashSet::new(),
            open: BTreeSet::new(),
        }
    }

    fn post_current_claims(
        &mut self,
        snapshot: &PublishedSnapshot,
        attention: &mut impl AttentionSink,
    ) -> Result<(), String> {
        for agent in &snapshot.agents {
            if agent.activity.state != "needs_attention" {
                continue;
            }
            let Some(observed_at) = agent.activity.observed_at_unix_ms else {
                continue;
            };
            let message = build_message(&self.machine, &snapshot.instance_id, agent, observed_at)?;
            let id = GlobalId {
                sender_id: message.sender_id.clone(),
                message_id: message.message_id.clone(),
            };
            if self.attempted.contains(&id) {
                continue;
            }
            let result = attention.post(&message)?;
            self.attempted.insert(id.clone());
            if result == PostResult::Accepted {
                self.open.insert(id);
            }
        }
        Ok(())
    }

    fn replace_open_messages(&mut self, messages: Vec<GlobalId>) {
        self.open = messages
            .into_iter()
            .filter(|message| message.sender_id.starts_with(&self.local_prefix))
            .collect();
    }

    fn reconcile(
        &mut self,
        snapshot: &PublishedSnapshot,
        attention: &mut impl AttentionSink,
        at: u64,
    ) -> Result<(), String> {
        if snapshot.instance_id != self.instance_id {
            return Err("Agentd instance changed; a fresh synchronization is required".to_owned());
        }
        if self
            .last_revision
            .is_some_and(|revision| snapshot.revision <= revision)
        {
            return Ok(());
        }
        self.last_revision = Some(snapshot.revision);
        if !snapshot.is_complete() {
            return Ok(());
        }

        let mut present = HashSet::new();
        for agent in &snapshot.agents {
            let sender_id = format!(
                "agentd://{}/{}/{}:{}",
                self.machine, snapshot.instance_id, agent.id.pid, agent.id.start_time_ticks
            );
            if !valid_sender_id(&sender_id) {
                return Err("Agentd identity cannot form a valid sender id".to_owned());
            }
            present.insert(sender_id.clone());
            match agent.activity.state.as_str() {
                "needs_attention" => {
                    let Some(observed_at) = agent.activity.observed_at_unix_ms else {
                        continue;
                    };
                    let message =
                        build_message(&self.machine, &snapshot.instance_id, agent, observed_at)?;
                    let current = GlobalId {
                        sender_id: message.sender_id.clone(),
                        message_id: message.message_id.clone(),
                    };
                    if !self.attempted.contains(&current) {
                        let result = attention.post(&message)?;
                        self.attempted.insert(current.clone());
                        if result == PostResult::Accepted {
                            self.open.insert(current.clone());
                        }
                    }
                    let previous: Vec<_> = self
                        .open
                        .iter()
                        .filter(|open| {
                            open.sender_id == sender_id && open.message_id != current.message_id
                        })
                        .cloned()
                        .collect();
                    for old in previous {
                        attention.lifecycle(&old, "superseded", at)?;
                        self.open.remove(&old);
                    }
                }
                "active" | "idle" => self.close_sender(&sender_id, "cleared", at, attention)?,
                _ => {}
            }
        }

        let absent: Vec<_> = self
            .open
            .iter()
            .filter(|open| {
                open.sender_id.starts_with(&self.local_prefix) && !present.contains(&open.sender_id)
            })
            .cloned()
            .collect();
        for id in absent {
            attention.lifecycle(&id, "cleared", at)?;
            self.open.remove(&id);
        }
        Ok(())
    }

    fn close_sender(
        &mut self,
        sender_id: &str,
        state: &str,
        at: u64,
        attention: &mut impl AttentionSink,
    ) -> Result<(), String> {
        let messages: Vec<_> = self
            .open
            .iter()
            .filter(|open| open.sender_id == sender_id)
            .cloned()
            .collect();
        for id in messages {
            attention.lifecycle(&id, state, at)?;
            self.open.remove(&id);
        }
        Ok(())
    }

    fn observe_attention_frame(&mut self, frame: &[u8]) -> Result<(), String> {
        let header = parse_attention_header(frame)?;
        if header.frame_type != "lifecycle" {
            return Ok(());
        }
        let lifecycle: LifecycleProjection = serde_json::from_slice(frame)
            .map_err(|error| format!("invalid attention lifecycle frame: {error}"))?;
        if lifecycle.sender_id.starts_with(&self.local_prefix) {
            self.open.remove(&GlobalId {
                sender_id: lifecycle.sender_id,
                message_id: lifecycle.message_id,
            });
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct LifecycleProjection {
    sender_id: String,
    message_id: String,
}

struct AttentionClient {
    writer: UnixStream,
    events: Receiver<BridgeEvent>,
    pending: VecDeque<BridgeEvent>,
    next_reference: u64,
}

impl AttentionClient {
    fn new(
        stream: UnixStream,
        event_tx: Sender<BridgeEvent>,
        events: Receiver<BridgeEvent>,
    ) -> Result<Self, String> {
        let reader_stream = stream
            .try_clone()
            .map_err(|error| format!("cloning attention socket: {error}"))?;
        thread::spawn(move || read_attention_frames(reader_stream, event_tx));
        Ok(Self {
            writer: stream,
            events,
            pending: VecDeque::new(),
            next_reference: 1,
        })
    }

    fn request(&mut self, mut frame: Value) -> Result<AttentionReply, RequestError> {
        let reference = format!("agentd-attention-{}", self.next_reference);
        self.next_reference = self
            .next_reference
            .checked_add(1)
            .ok_or_else(|| RequestError::Protocol("request reference exhausted".to_owned()))?;
        let object = frame
            .as_object_mut()
            .ok_or_else(|| RequestError::Protocol("outbound frame is not an object".to_owned()))?;
        object.insert("ref".to_owned(), Value::String(reference.clone()));
        let encoded = serde_json::to_vec(&frame).map_err(|error| {
            RequestError::Protocol(format!("encoding attention frame: {error}"))
        })?;
        self.writer
            .write_all(&encoded)
            .and_then(|()| self.writer.write_all(b"\n"))
            .and_then(|()| self.writer.flush())
            .map_err(|error| {
                RequestError::Disconnected(format!("writing attention frame: {error}"))
            })?;

        let deadline = Instant::now() + REQUEST_TIMEOUT;
        let mut deferred = VecDeque::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.pending.extend(deferred);
                return Err(RequestError::Disconnected(
                    "attention daemon did not reply within 10 seconds".to_owned(),
                ));
            }
            match self.events.recv_timeout(remaining.min(EVENT_POLL)) {
                Ok(BridgeEvent::AttentionFrame(raw)) => {
                    let header = match parse_attention_header(&raw) {
                        Ok(header) => header,
                        Err(error) => {
                            self.pending.extend(deferred);
                            return Err(RequestError::Protocol(error));
                        }
                    };
                    if header.reference.as_deref() == Some(reference.as_str()) {
                        self.pending.extend(deferred);
                        return match header.frame_type.as_str() {
                            "ok" => Ok(AttentionReply {
                                duplicate: header.duplicate,
                            }),
                            "error" => Err(RequestError::Rejected {
                                reason: header.reason.unwrap_or_else(|| "unknown".to_owned()),
                            }),
                            other => Err(RequestError::Protocol(format!(
                                "attention daemon replied with {other:?}"
                            ))),
                        };
                    }
                    deferred.push_back(BridgeEvent::AttentionFrame(raw));
                }
                Ok(event @ BridgeEvent::AgentdSnapshot(_)) => deferred.push_back(event),
                Ok(BridgeEvent::AgentdEnded) => {
                    self.pending.extend(deferred);
                    return Err(RequestError::Disconnected(
                        "Agentd watch disconnected".to_owned(),
                    ));
                }
                Ok(BridgeEvent::AttentionEnded) => {
                    self.pending.extend(deferred);
                    return Err(RequestError::Disconnected(
                        "attention daemon disconnected".to_owned(),
                    ));
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    self.pending.extend(deferred);
                    return Err(RequestError::Disconnected(
                        "local event readers stopped".to_owned(),
                    ));
                }
            }
        }
    }

    fn read_subscribed_snapshot(&mut self) -> Result<Vec<GlobalId>, String> {
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        let mut deferred = VecDeque::new();
        loop {
            let event = if let Some(event) = self.pending.pop_front() {
                event
            } else {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    self.pending.extend(deferred);
                    return Err("attention daemon did not send a subscription snapshot".to_owned());
                }
                match self.events.recv_timeout(remaining.min(EVENT_POLL)) {
                    Ok(event) => event,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => {
                        self.pending.extend(deferred);
                        return Err("local event readers stopped".to_owned());
                    }
                }
            };
            match event {
                BridgeEvent::AttentionFrame(raw) => {
                    let header = match parse_attention_header(&raw) {
                        Ok(header) => header,
                        Err(error) => {
                            self.pending.extend(deferred);
                            return Err(error);
                        }
                    };
                    if header.frame_type == "snapshot" {
                        let snapshot: AttentionSnapshot = serde_json::from_slice(&raw)
                            .map_err(|error| format!("invalid attention snapshot: {error}"))?;
                        self.pending.extend(deferred);
                        return Ok(snapshot
                            .messages
                            .into_iter()
                            .map(|entry| GlobalId {
                                sender_id: entry.message.sender_id,
                                message_id: entry.message.message_id,
                            })
                            .collect());
                    }
                    deferred.push_back(BridgeEvent::AttentionFrame(raw));
                }
                event @ BridgeEvent::AgentdSnapshot(_) => deferred.push_back(event),
                BridgeEvent::AgentdEnded => {
                    self.pending.extend(deferred);
                    return Err("Agentd watch disconnected".to_owned());
                }
                BridgeEvent::AttentionEnded => {
                    self.pending.extend(deferred);
                    return Err("attention daemon disconnected".to_owned());
                }
            }
        }
    }
}

impl AttentionSink for AttentionClient {
    fn post(&mut self, message: &BridgeMessage) -> Result<PostResult, String> {
        let frame = serde_json::to_value(message)
            .map_err(|error| format!("encoding attention message: {error}"))?;
        let reply = self.request(frame).map_err(|error| error.to_string())?;
        Ok(if reply.duplicate.unwrap_or(false) {
            PostResult::Duplicate
        } else {
            PostResult::Accepted
        })
    }

    fn lifecycle(&mut self, id: &GlobalId, state: &str, at: u64) -> Result<(), String> {
        let frame = json!({
            "v": 1,
            "type": "lifecycle",
            "sender_id": id.sender_id,
            "message_id": id.message_id,
            "state": state,
            "at": at,
        });
        match self.request(frame) {
            Ok(_) => Ok(()),
            Err(RequestError::Rejected { reason })
                if reason == "closed" || reason == "unknown_message" =>
            {
                Ok(())
            }
            Err(error) => Err(error.to_string()),
        }
    }

    fn subscribe_empty_snapshot(&mut self) -> Result<Vec<GlobalId>, String> {
        self.request(json!({"v": 1, "type": "subscribe", "sender_ids": []}))
            .map_err(|error| error.to_string())?;
        self.read_subscribed_snapshot()
    }
}

struct AttentionReply {
    duplicate: Option<bool>,
}

#[derive(Debug)]
enum RequestError {
    Disconnected(String),
    Rejected { reason: String },
    Protocol(String),
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disconnected(reason) => formatter.write_str(reason),
            Self::Rejected { reason } => {
                write!(formatter, "attention daemon rejected the request: {reason}")
            }
            Self::Protocol(reason) => write!(formatter, "attention protocol error: {reason}"),
        }
    }
}

#[derive(Deserialize)]
struct AttentionHeader {
    v: u8,
    #[serde(rename = "type")]
    frame_type: String,
    #[serde(rename = "ref")]
    reference: Option<String>,
    reason: Option<String>,
    duplicate: Option<bool>,
}

#[derive(Deserialize)]
struct AttentionSnapshot {
    messages: Vec<AttentionSnapshotEntry>,
}

#[derive(Deserialize)]
struct AttentionSnapshotEntry {
    message: AttentionMessageId,
}

#[derive(Deserialize)]
struct AttentionMessageId {
    sender_id: String,
    message_id: String,
}

fn parse_attention_header(frame: &[u8]) -> Result<AttentionHeader, String> {
    if !frame.ends_with(b"\n") {
        return Err("attention frame was not newline terminated".to_owned());
    }
    let header: AttentionHeader = serde_json::from_slice(frame)
        .map_err(|error| format!("invalid attention protocol frame: {error}"))?;
    if header.v != 1 {
        return Err(format!("unsupported attention frame version {}", header.v));
    }
    Ok(header)
}

#[derive(Debug)]
enum BridgeEvent {
    AgentdSnapshot(Result<PublishedSnapshot, String>),
    AgentdEnded,
    AttentionFrame(Vec<u8>),
    AttentionEnded,
}

struct AgentdWatch {
    child: Child,
}

impl AgentdWatch {
    fn start(events: Sender<BridgeEvent>) -> Result<Self, String> {
        let mut child = Command::new("agentd")
            .args(["watch", "--json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("starting local agentd watch --json: {error}"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Agentd watch stdout was not captured".to_owned())?;
        thread::spawn(move || read_agentd_frames(stdout, events));
        Ok(Self { child })
    }
}

impl Drop for AgentdWatch {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn read_agentd_frames(stdout: impl std::io::Read, events: Sender<BridgeEvent>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut frame = Vec::new();
        match reader.read_until(b'\n', &mut frame) {
            Ok(0) => {
                let _ = events.send(BridgeEvent::AgentdEnded);
                return;
            }
            Ok(_) if frame.ends_with(b"\n") => {
                if events
                    .send(BridgeEvent::AgentdSnapshot(parse_agentd_snapshot(&frame)))
                    .is_err()
                {
                    return;
                }
            }
            Ok(_) => {
                if events
                    .send(BridgeEvent::AgentdSnapshot(Err(
                        "Agentd snapshot was not newline terminated".to_owned(),
                    )))
                    .is_err()
                {
                    return;
                }
            }
            Err(error) => {
                let _ = events.send(BridgeEvent::AgentdSnapshot(Err(format!(
                    "reading Agentd watch output: {error}"
                ))));
                let _ = events.send(BridgeEvent::AgentdEnded);
                return;
            }
        }
    }
}

fn read_attention_frames(stream: UnixStream, events: Sender<BridgeEvent>) {
    let mut reader = BufReader::new(stream);
    loop {
        let mut frame = Vec::new();
        match reader.read_until(b'\n', &mut frame) {
            Ok(0) => {
                let _ = events.send(BridgeEvent::AttentionEnded);
                return;
            }
            Ok(_) => {
                if events.send(BridgeEvent::AttentionFrame(frame)).is_err() {
                    return;
                }
            }
            Err(_) => {
                let _ = events.send(BridgeEvent::AttentionEnded);
                return;
            }
        }
    }
}

#[derive(Default)]
struct Backoff {
    next: Duration,
}

impl Backoff {
    fn next(&mut self) -> Duration {
        if self.next.is_zero() {
            self.next = RETRY_START;
        }
        let delay = self.next;
        self.next = self.next.saturating_mul(2).min(RETRY_MAX);
        delay
    }

    fn reset(&mut self) {
        self.next = Duration::ZERO;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(revision: u64, scan: &str, agents: Vec<PublishedAgent>) -> PublishedSnapshot {
        PublishedSnapshot {
            frame_type: "snapshot".to_owned(),
            schema: "agentd.snapshot.v1".to_owned(),
            instance_id: "inst-1".to_owned(),
            revision,
            scan: PublishedScan {
                state: scan.to_owned(),
            },
            agents,
        }
    }

    fn agent(
        state: &str,
        observed_at: Option<u64>,
        name: Option<&str>,
        tmux_session: Option<&str>,
    ) -> PublishedAgent {
        PublishedAgent {
            id: PublishedAgentId {
                pid: 48211,
                start_time_ticks: 9127734,
            },
            harness: "claude".to_owned(),
            activity: PublishedActivity {
                state: state.to_owned(),
                observed_at_unix_ms: observed_at,
            },
            name: name.map(str::to_owned),
            tmux: tmux_session.map(|session| PublishedTmux {
                session: session.to_owned(),
            }),
        }
    }

    #[derive(Default)]
    struct RecordingSink {
        open: BTreeSet<GlobalId>,
        closed: HashSet<GlobalId>,
        events: Vec<String>,
    }

    impl AttentionSink for RecordingSink {
        fn post(&mut self, message: &BridgeMessage) -> Result<PostResult, String> {
            let id = GlobalId {
                sender_id: message.sender_id.clone(),
                message_id: message.message_id.clone(),
            };
            self.events.push(format!("post:{}", message.message_id));
            if self.open.contains(&id) || self.closed.contains(&id) {
                return Ok(PostResult::Duplicate);
            }
            self.open.insert(id);
            Ok(PostResult::Accepted)
        }

        fn lifecycle(&mut self, id: &GlobalId, state: &str, _at: u64) -> Result<(), String> {
            self.events.push(format!("{state}:{}", id.message_id));
            self.open.remove(id);
            self.closed.insert(id.clone());
            Ok(())
        }

        fn subscribe_empty_snapshot(&mut self) -> Result<Vec<GlobalId>, String> {
            self.events.push("subscribe-empty".to_owned());
            Ok(self.open.iter().cloned().collect())
        }
    }

    fn identity(timestamp: u64) -> GlobalId {
        GlobalId {
            sender_id: "agentd://example-host/inst-1/48211:9127734".to_owned(),
            message_id: format!("needs_attention@{timestamp}"),
        }
    }

    #[test]
    fn machine_name_normalizes_once_and_rejects_unrepresentable_segments() {
        assert_eq!(normalize_machine("Example-Host.").unwrap(), "example-host");
        assert!(normalize_machine("bad/host").is_err());
        assert!(normalize_machine("bad:host").is_err());
        assert!(normalize_machine("bad host").is_err());
        assert!(normalize_machine(".").is_err());
    }

    #[test]
    fn message_uses_exact_identity_title_priority_and_open_command_without_body() {
        let with_name = build_message(
            "example-host",
            "inst-1",
            &agent(
                "needs_attention",
                Some(1791440000000),
                Some("API review"),
                Some("api"),
            ),
            1791440000000,
        )
        .unwrap();
        assert_eq!(
            with_name.sender_id,
            "agentd://example-host/inst-1/48211:9127734"
        );
        assert_eq!(with_name.message_id, "needs_attention@1791440000000");
        assert_eq!(with_name.created_at, 1791440000000);
        assert_eq!(with_name.title, "API review on example-host");
        assert_eq!(
            with_name.open,
            Some(OpenCommand {
                argv: vec![
                    "tmux".to_owned(),
                    "attach-session".to_owned(),
                    "-t".to_owned(),
                    "=api".to_owned(),
                ],
                terminal: true,
            })
        );
        let encoded = serde_json::to_value(with_name).unwrap();
        assert!(encoded.get("body").is_none());
        assert!(encoded.get("paneId").is_none());

        let fallback = build_message(
            "example-host",
            "inst-1",
            &agent("needs_attention", Some(1), None, Some("build")),
            1,
        )
        .unwrap();
        assert_eq!(fallback.title, "build on example-host");
        let harness = build_message(
            "example-host",
            "inst-1",
            &agent("needs_attention", Some(1), None, None),
            1,
        )
        .unwrap();
        assert_eq!(harness.title, "claude on example-host");
        assert!(harness.open.is_none());
    }

    #[test]
    fn initial_sync_posts_before_subscribing_and_superseding_old_episode() {
        let old = identity(100);
        let mut sink = RecordingSink::default();
        sink.open.insert(old.clone());
        let current = snapshot(
            2,
            "complete",
            vec![agent("needs_attention", Some(200), None, Some("api"))],
        );
        let state = initial_sync("example-host", &current, &mut sink).unwrap();
        assert_eq!(
            sink.events,
            vec![
                "post:needs_attention@200".to_owned(),
                "subscribe-empty".to_owned(),
                "superseded:needs_attention@100".to_owned(),
            ]
        );
        assert!(state.open.contains(&identity(200)));
        assert!(!state.open.contains(&old));
    }

    #[test]
    fn incomplete_unknown_and_null_timestamp_do_not_change_open_messages() {
        let mut state = BridgeState::new("example-host", "inst-1");
        let old = identity(100);
        state.open.insert(old.clone());
        let mut sink = RecordingSink::default();

        state
            .reconcile(
                &snapshot(1, "degraded", vec![agent("active", Some(2), None, None)]),
                &mut sink,
                500,
            )
            .unwrap();
        state
            .reconcile(
                &snapshot(2, "complete", vec![agent("unknown", None, None, None)]),
                &mut sink,
                500,
            )
            .unwrap();
        state
            .reconcile(
                &snapshot(
                    3,
                    "complete",
                    vec![agent("needs_attention", None, None, None)],
                ),
                &mut sink,
                500,
            )
            .unwrap();
        assert!(sink.events.is_empty());
        assert!(state.open.contains(&old));
    }

    #[test]
    fn active_idle_and_complete_absence_clear_local_messages_only() {
        let mut state = BridgeState::new("example-host", "inst-1");
        let local = identity(100);
        let foreign = GlobalId {
            sender_id: "agentd://other-host/inst-9/9:10".to_owned(),
            message_id: "needs_attention@100".to_owned(),
        };
        state.open.insert(local.clone());
        state.open.insert(foreign.clone());
        let mut sink = RecordingSink::default();

        state
            .reconcile(
                &snapshot(1, "complete", vec![agent("active", Some(2), None, None)]),
                &mut sink,
                500,
            )
            .unwrap();
        assert_eq!(sink.events, vec!["cleared:needs_attention@100".to_owned()]);
        assert!(!state.open.contains(&local));
        assert!(state.open.contains(&foreign));

        state.open.insert(local.clone());
        state
            .reconcile(
                &snapshot(2, "complete", vec![agent("idle", Some(3), None, None)]),
                &mut sink,
                501,
            )
            .unwrap();
        assert_eq!(
            sink.events.last().unwrap().as_str(),
            "cleared:needs_attention@100"
        );

        state.open.insert(local.clone());
        state
            .reconcile(&snapshot(3, "complete", vec![]), &mut sink, 502)
            .unwrap();
        assert!(!state.open.contains(&local));
        assert!(state.open.contains(&foreign));
    }

    #[test]
    fn answered_duplicate_stays_closed_and_answer_frames_are_ignored() {
        let current = snapshot(
            1,
            "complete",
            vec![agent("needs_attention", Some(100), None, None)],
        );
        let mut sink = RecordingSink::default();
        let closed = identity(100);
        sink.closed.insert(closed.clone());
        let state = initial_sync("example-host", &current, &mut sink).unwrap();
        assert!(!state.open.contains(&closed));
        assert_eq!(sink.events[0], "post:needs_attention@100");

        let mut state = BridgeState::new("example-host", "inst-1");
        state.open.insert(identity(100));
        state
            .observe_attention_frame(
                br#"{"v":1,"type":"answer","sender_id":"agentd://example-host/inst-1/48211:9127734","message_id":"needs_attention@100","answer":null}"#,
            )
            .unwrap();
        state
            .observe_attention_frame(
                br#"{"v":1,"type":"lifecycle","sender_id":"agentd://other-host/inst-9/9:10","message_id":"needs_attention@100","state":"cleared","at":1}"#,
            )
            .unwrap();
        assert!(state.open.contains(&identity(100)));
    }

    #[test]
    fn attention_transport_sends_versioned_line_and_waits_for_matching_ack() {
        let (client_stream, mut daemon_stream) = UnixStream::pair().unwrap();
        let (event_tx, event_rx) = mpsc::channel();
        let mut client = AttentionClient::new(client_stream, event_tx, event_rx).unwrap();
        let daemon = thread::spawn(move || {
            let mut reader = BufReader::new(daemon_stream.try_clone().unwrap());
            let mut line = Vec::new();
            reader.read_until(b'\n', &mut line).unwrap();
            let request: Value = serde_json::from_slice(&line).unwrap();
            assert_eq!(request["v"], 1);
            assert_eq!(request["type"], "message");
            assert!(request.get("body").is_none());
            let reply = json!({"v": 1, "type": "ok", "ref": request["ref"]});
            writeln!(daemon_stream, "{reply}").unwrap();
        });
        let message = build_message(
            "example-host",
            "inst-1",
            &agent("needs_attention", Some(77), None, None),
            77,
        )
        .unwrap();
        assert_eq!(client.post(&message).unwrap(), PostResult::Accepted);
        daemon.join().unwrap();
    }

    #[test]
    fn subscribe_uses_empty_set_and_reads_the_daemon_snapshot() {
        let (client_stream, mut daemon_stream) = UnixStream::pair().unwrap();
        let (event_tx, event_rx) = mpsc::channel();
        let mut client = AttentionClient::new(client_stream, event_tx, event_rx).unwrap();
        let daemon = thread::spawn(move || {
            let mut reader = BufReader::new(daemon_stream.try_clone().unwrap());
            let mut line = Vec::new();
            reader.read_until(b'\n', &mut line).unwrap();
            let request: Value = serde_json::from_slice(&line).unwrap();
            assert_eq!(request["type"], "subscribe");
            assert_eq!(request["sender_ids"], json!([]));
            let reference = request["ref"].clone();
            writeln!(
                daemon_stream,
                "{}",
                json!({"v": 1, "type": "ok", "ref": reference})
            )
            .unwrap();
            writeln!(daemon_stream, "{}", json!({
                "v": 1,
                "type": "snapshot",
                "messages": [
                    {"as": "knock", "message": {"sender_id": "agentd://example-host/inst-1/1:2", "message_id": "needs_attention@1", "title": "local"}},
                    {"as": "knock", "message": {"sender_id": "agentd://other-host/inst-9/9:10", "message_id": "needs_attention@2", "title": "foreign"}}
                ]
            })).unwrap();
        });
        let messages = client.subscribe_empty_snapshot().unwrap();
        assert_eq!(messages.len(), 2);
        assert!(
            messages
                .iter()
                .any(|id| id.sender_id == "agentd://other-host/inst-9/9:10")
        );
        daemon.join().unwrap();
    }
}
