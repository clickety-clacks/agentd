use crate::model::{ActivityState, AgentId, CwdState, PresenceState, ScanIssue, Snapshot};
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    UnsupportedVersion,
    UnknownOperation,
    MalformedRequest,
    RequestTooLarge,
    InvalidActivity,
    UnknownAgent,
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
    let mut duplicate_check = serde_json::Deserializer::from_slice(input);
    NoDuplicateKeys
        .deserialize(&mut duplicate_check)
        .map_err(|_| ErrorCode::MalformedRequest)?;
    duplicate_check
        .end()
        .map_err(|_| ErrorCode::MalformedRequest)?;

    let value: Value = serde_json::from_slice(input).map_err(|_| ErrorCode::MalformedRequest)?;
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
        "snapshot" | "subscribe" => Err(ErrorCode::MalformedRequest),
        _ if exact_keys(object.keys(), &["op", "version"]) => Err(ErrorCode::UnknownOperation),
        _ => Err(ErrorCode::MalformedRequest),
    }
}

fn parse_activity(object: &serde_json::Map<String, Value>) -> Result<Request, ErrorCode> {
    if !exact_keys(object.keys(), &["agent", "op", "state", "version"]) {
        return Err(ErrorCode::MalformedRequest);
    }
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
    let state = match object.get("state").and_then(Value::as_str) {
        Some("active") => ActivityState::Active,
        Some("idle") => ActivityState::Idle,
        _ => return Err(ErrorCode::InvalidActivity),
    };
    Ok(Request::Activity {
        agent: AgentId {
            pid,
            start_time_ticks,
        },
        state,
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
        ErrorCode::InvalidActivity => "activity state must be active or idle",
        ErrorCode::UnknownAgent => "agent identity is not present",
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
            ActivityState::Unknown => "unknown",
        };
        output.push_str(&format!(
            "agent pid={} startTimeTicks={} harness={} presence={} cwd={} activity={}\n",
            agent.id.pid,
            agent.id.start_time_ticks,
            agent.harness.as_str(),
            presence,
            cwd,
            activity
        ));
    }
    output
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
}
