use crate::model::{ActivityState, AgentId, CwdState, PresenceState, ScanIssue, Snapshot};
use crate::names::validate_display_name;
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserializer, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Request {
    Snapshot,
    Subscribe,
    Activity {
        agent: AgentId,
        state: ActivityState,
    },
    Name {
        agent: AgentId,
        name: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    UnsupportedVersion,
    UnknownOperation,
    MalformedRequest,
    RequestTooLarge,
    InvalidActivity,
    InvalidName,
    UnknownAgent,
    NameStoreUnavailable,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AckFrame<'a> {
    #[serde(rename = "type")]
    pub frame_type: &'static str,
    pub instance_id: &'a str,
    pub revision: u64,
}

#[derive(Serialize)]
pub struct ErrorFrame {
    #[serde(rename = "type")]
    pub frame_type: &'static str,
    pub code: ErrorCode,
    pub message: &'static str,
}

pub fn parse_request(input: &[u8]) -> Result<Request, ErrorCode> {
    let value = parse_json_value(input).map_err(|_| ErrorCode::MalformedRequest)?;
    let object = value.as_object().ok_or(ErrorCode::MalformedRequest)?;
    let version = object
        .get("version")
        .and_then(Value::as_u64)
        .ok_or(ErrorCode::MalformedRequest)?;
    if version != 1 {
        return Err(ErrorCode::UnsupportedVersion);
    }
    let operation = object
        .get("op")
        .and_then(Value::as_str)
        .ok_or(ErrorCode::MalformedRequest)?;
    match operation {
        "snapshot" if exact_keys(object.keys(), &["op", "version"]) => Ok(Request::Snapshot),
        "subscribe" if exact_keys(object.keys(), &["op", "version"]) => Ok(Request::Subscribe),
        "activity" => parse_activity(object),
        "name" => parse_name(object),
        "snapshot" | "subscribe" => Err(ErrorCode::MalformedRequest),
        _ if exact_keys(object.keys(), &["op", "version"]) => Err(ErrorCode::UnknownOperation),
        _ => Err(ErrorCode::MalformedRequest),
    }
}

fn parse_name(object: &serde_json::Map<String, Value>) -> Result<Request, ErrorCode> {
    if !exact_keys(object.keys(), &["agent", "name", "op", "version"]) {
        return Err(ErrorCode::MalformedRequest);
    }
    let agent = parse_agent(object)?;
    let name = match object.get("name") {
        Some(Value::Null) => None,
        Some(Value::String(name)) if validate_display_name(name) => Some(name.clone()),
        Some(Value::String(_)) => return Err(ErrorCode::InvalidName),
        _ => return Err(ErrorCode::MalformedRequest),
    };
    Ok(Request::Name { agent, name })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JsonParseError;

pub fn parse_json_value(input: &[u8]) -> Result<Value, JsonParseError> {
    let mut duplicate_check = serde_json::Deserializer::from_slice(input);
    NoDuplicateKeys
        .deserialize(&mut duplicate_check)
        .map_err(|_| JsonParseError)?;
    duplicate_check.end().map_err(|_| JsonParseError)?;
    serde_json::from_slice(input).map_err(|_| JsonParseError)
}

fn parse_activity(object: &serde_json::Map<String, Value>) -> Result<Request, ErrorCode> {
    if !exact_keys(object.keys(), &["agent", "op", "state", "version"]) {
        return Err(ErrorCode::MalformedRequest);
    }
    let agent = parse_agent(object)?;
    let state = match object.get("state").and_then(Value::as_str) {
        Some("active") => ActivityState::Active,
        Some("idle") => ActivityState::Idle,
        Some("needs_attention") => ActivityState::NeedsAttention,
        _ => return Err(ErrorCode::InvalidActivity),
    };
    Ok(Request::Activity { agent, state })
}

fn parse_agent(object: &serde_json::Map<String, Value>) -> Result<AgentId, ErrorCode> {
    let agent = object
        .get("agent")
        .and_then(Value::as_object)
        .ok_or(ErrorCode::MalformedRequest)?;
    if !exact_keys(agent.keys(), &["pid", "startTimeTicks"]) {
        return Err(ErrorCode::MalformedRequest);
    }
    let pid = agent
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or(ErrorCode::MalformedRequest)?;
    let start_time_ticks = agent
        .get("startTimeTicks")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or(ErrorCode::MalformedRequest)?;
    Ok(AgentId {
        pid,
        start_time_ticks,
    })
}

fn exact_keys<'a>(keys: impl Iterator<Item = &'a String>, expected: &[&str]) -> bool {
    let actual: HashSet<&str> = keys.map(String::as_str).collect();
    let expected: HashSet<&str> = expected.iter().copied().collect();
    actual == expected
}

pub fn error_message(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::UnsupportedVersion => "unsupported protocol version",
        ErrorCode::UnknownOperation => "unknown operation",
        ErrorCode::MalformedRequest => "malformed request",
        ErrorCode::RequestTooLarge => "request exceeds 65536 bytes",
        ErrorCode::InvalidActivity => "activity state must be active, idle, or needs_attention",
        ErrorCode::InvalidName => "display name must be 1 to 64 UTF-8 bytes without controls",
        ErrorCode::UnknownAgent => "agent identity is not present",
        ErrorCode::NameStoreUnavailable => "name store is unavailable",
    }
}

pub fn human_snapshot(snapshot: &Snapshot) -> String {
    let mut output = format!(
        "instance={} revision={} scan={} agents={}\n",
        snapshot.instance_id,
        snapshot.revision,
        scan_state(snapshot),
        snapshot.agents.len()
    );
    for issue in &snapshot.scan.issues {
        output.push_str(&human_issue(issue));
    }
    for agent in &snapshot.agents {
        let presence = match agent.presence.state {
            PresenceState::Present => "present",
            PresenceState::Unknown => "unknown",
        };
        let cwd = match agent.cwd.state {
            CwdState::Known => agent.cwd.value.as_deref().unwrap_or("unknown"),
            CwdState::Unknown => "unknown",
        };
        let activity = match agent.activity.state {
            ActivityState::Active => "active",
            ActivityState::Idle => "idle",
            ActivityState::NeedsAttention => "needs_attention",
            ActivityState::Unknown => "unknown",
        };
        let tmux = agent.tmux.as_ref().map(|location| {
            format!(
                "{}:{}.{}:{}",
                location.session, location.window_index, location.window_name, location.pane_id
            )
        });
        let cwd_base = match agent.cwd.state {
            CwdState::Known => agent.cwd.value.as_deref().and_then(cwd_base),
            CwdState::Unknown => None,
        };
        output.push_str(&format!(
            "agent name={} tmux={} cwdBase={} pid={} startTimeTicks={} startedAtUnixMs={} tty={} harness={} presence={} cwd={} activity={}\n",
            json_optional(agent.name.as_deref()),
            json_optional(tmux.as_deref()),
            json_optional(cwd_base.as_deref()),
            agent.id.pid,
            agent.id.start_time_ticks,
            agent
                .started_at_unix_ms
                .map_or_else(|| "null".to_owned(), |value| value.to_string()),
            json_optional(agent.tty.as_deref()),
            agent.harness.as_str(),
            presence,
            cwd,
            activity
        ));
    }
    output
}

fn cwd_base(value: &str) -> Option<String> {
    if value == "/" {
        Some("/".into())
    } else {
        std::path::Path::new(value)
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
    }
}

fn json_optional(value: Option<&str>) -> String {
    value.map_or_else(
        || "null".to_owned(),
        |value| serde_json::to_string(value).expect("serializing display value cannot fail"),
    )
}

fn human_issue(issue: &ScanIssue) -> String {
    let pid = issue
        .pid
        .map_or_else(|| "none".to_owned(), |pid| pid.to_string());
    let field = issue.field.as_str();
    let cause = issue.cause.as_str();
    format!("issue pid={pid} field={field} cause={cause}\n")
}

fn scan_state(snapshot: &Snapshot) -> &'static str {
    match snapshot.scan.state {
        crate::model::ScanState::Complete => "complete",
        crate::model::ScanState::Degraded => "degraded",
    }
}

struct NoDuplicateKeys;

impl<'de> DeserializeSeed<'de> for NoDuplicateKeys {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(NoDuplicateVisitor)
    }
}

struct NoDuplicateVisitor;

impl<'de> Visitor<'de> for NoDuplicateVisitor {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JSON without duplicate object keys")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key) {
                return Err(de::Error::custom("duplicate object key"));
            }
            map.next_value_seed(NoDuplicateKeys)?;
        }
        Ok(())
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element_seed(NoDuplicateKeys)?.is_some() {}
        Ok(())
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_string<E>(self, _value: String) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(NoDuplicateVisitor)
    }

    fn visit_bytes<E>(self, _value: &[u8]) -> Result<Self::Value, E> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Scan, ScanIssue, ScanState, SnapshotReason, SnapshotSchema, SnapshotType};

    #[test]
    fn parses_closed_request_set() {
        assert_eq!(
            parse_request(br#"{"version":1,"op":"snapshot"}"#),
            Ok(Request::Snapshot)
        );
        assert_eq!(
            parse_request(br#"{"version":1,"op":"subscribe"}"#),
            Ok(Request::Subscribe)
        );
        assert!(matches!(
            parse_request(
                br#"{"version":1,"op":"activity","agent":{"pid":7,"startTimeTicks":9},"state":"active"}"#
            ),
            Ok(Request::Activity { .. })
        ));
        assert_eq!(
            parse_request(
                br#"{"version":1,"op":"name","agent":{"pid":7,"startTimeTicks":9},"name":"Agentd spec"}"#
            ),
            Ok(Request::Name {
                agent: AgentId {
                    pid: 7,
                    start_time_ticks: 9
                },
                name: Some("Agentd spec".into())
            })
        );
        assert!(matches!(
            parse_request(
                br#"{"version":1,"op":"name","agent":{"pid":7,"startTimeTicks":9},"name":null}"#
            ),
            Ok(Request::Name { name: None, .. })
        ));
        assert!(matches!(
            parse_request(
                br#"{"version":1,"op":"activity","agent":{"pid":7,"startTimeTicks":9},"state":"needs_attention"}"#
            ),
            Ok(Request::Activity {
                state: ActivityState::NeedsAttention,
                ..
            })
        ));
    }

    #[test]
    fn rejects_duplicate_nested_and_extra_fields() {
        assert_eq!(
            parse_request(br#"{"version":1,"version":1,"op":"snapshot"}"#),
            Err(ErrorCode::MalformedRequest)
        );
        assert_eq!(
            parse_request(
                br#"{"version":1,"op":"activity","agent":{"pid":7,"pid":8,"startTimeTicks":9},"state":"active"}"#
            ),
            Err(ErrorCode::MalformedRequest)
        );
        assert_eq!(
            parse_request(br#"{"version":1,"op":"snapshot","extra":true}"#),
            Err(ErrorCode::MalformedRequest)
        );
    }

    #[test]
    fn classifies_closed_errors() {
        assert_eq!(
            parse_request(br#"{"version":2,"op":"snapshot"}"#),
            Err(ErrorCode::UnsupportedVersion)
        );
        assert_eq!(
            parse_request(
                br#"{"version":1,"op":"name","agent":{"pid":7,"startTimeTicks":9},"name":"line\nbreak"}"#
            ),
            Err(ErrorCode::InvalidName)
        );
        assert_eq!(
            parse_request(br#"{"version":1,"op":"history"}"#),
            Err(ErrorCode::UnknownOperation)
        );
        assert_eq!(
            parse_request(
                br#"{"version":1,"op":"activity","agent":{"pid":7,"startTimeTicks":9},"state":"unknown"}"#
            ),
            Err(ErrorCode::InvalidActivity)
        );
    }

    #[test]
    fn degraded_empty_human_snapshot_is_truthful_and_success_shaped() {
        let snapshot = Snapshot {
            frame_type: SnapshotType::Snapshot,
            reason: SnapshotReason::Initial,
            schema: SnapshotSchema::V1,
            instance_id: "0123456789abcdef0123456789abcdef".into(),
            revision: 1,
            observed_at_unix_ms: 1,
            scan: Scan {
                state: ScanState::Degraded,
                issues: vec![ScanIssue {
                    pid: None,
                    field: crate::model::IssueField::Proc,
                    cause: crate::model::IssueCause::ProcUnavailable,
                }],
            },
            agents: Vec::new(),
        };
        assert_eq!(
            human_snapshot(&snapshot),
            "instance=0123456789abcdef0123456789abcdef revision=1 scan=degraded agents=0\nissue pid=none field=proc cause=proc_unavailable\n"
        );
    }

    #[test]
    fn v02_agent_frames_decode_new_fields_as_null() {
        let agent: crate::model::AgentRecord = serde_json::from_str(
            r#"{"id":{"pid":7,"startTimeTicks":9},"harness":"codex","detectedBy":"proc_comm","presence":{"state":"present","cause":null},"cwd":{"state":"known","value":"/work","cause":null},"activity":{"state":"unknown","source":"none","observedAtUnixMs":null}}"#,
        )
        .unwrap();
        assert_eq!(agent.tty, None);
        assert_eq!(agent.tmux, None);
        assert_eq!(agent.name, None);
        assert_eq!(agent.started_at_unix_ms, None);
    }

    #[test]
    fn human_agent_line_leads_with_identity_fields_and_keeps_raw_cwd() {
        use crate::model::{
            Activity, AgentRecord, Cwd, DetectedBy, Harness, Presence, TmuxLocation,
        };
        let snapshot = Snapshot {
            frame_type: SnapshotType::Snapshot,
            reason: SnapshotReason::Initial,
            schema: SnapshotSchema::V1,
            instance_id: "0123456789abcdef0123456789abcdef".into(),
            revision: 1,
            observed_at_unix_ms: 1,
            scan: Scan {
                state: ScanState::Complete,
                issues: Vec::new(),
            },
            agents: vec![AgentRecord {
                id: AgentId {
                    pid: 7,
                    start_time_ticks: 9,
                },
                harness: Harness::Codex,
                detected_by: DetectedBy::ProcComm,
                presence: Presence::present(),
                cwd: Cwd::known("/home/mike/work/agentd".into()),
                activity: Activity::unknown(),
                tty: Some("pts/13".into()),
                tmux: Some(TmuxLocation {
                    session: "agents".into(),
                    window_index: 2,
                    window_name: "spec".into(),
                    pane_id: "%7".into(),
                }),
                name: Some("Agentd spec".into()),
                started_at_unix_ms: Some(1_700_000_002_500),
            }],
        };
        assert_eq!(
            human_snapshot(&snapshot),
            "instance=0123456789abcdef0123456789abcdef revision=1 scan=complete agents=1\nagent name=\"Agentd spec\" tmux=\"agents:2.spec:%7\" cwdBase=\"agentd\" pid=7 startTimeTicks=9 startedAtUnixMs=1700000002500 tty=\"pts/13\" harness=codex presence=present cwd=/home/mike/work/agentd activity=unknown\n"
        );
    }
}
