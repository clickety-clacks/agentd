use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AgentId {
    pub pid: u32,
    pub start_time_ticks: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Harness {
    Codex,
    Claude,
}

impl Harness {
    pub fn from_comm(comm: &str) -> Option<Self> {
        match comm {
            "codex" => Some(Self::Codex),
            "claude" | "claude.exe" => Some(Self::Claude),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueField {
    Proc,
    Stat,
    Status,
    ParentChain,
    Cwd,
}

impl IssueField {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proc => "proc",
            Self::Stat => "stat",
            Self::Status => "status",
            Self::ParentChain => "parent_chain",
            Self::Cwd => "cwd",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueCause {
    PermissionDenied,
    ProcessRaced,
    IoError,
    ProcUnavailable,
}

impl IssueCause {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PermissionDenied => "permission_denied",
            Self::ProcessRaced => "process_raced",
            Self::IoError => "io_error",
            Self::ProcUnavailable => "proc_unavailable",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScanIssue {
    pub pid: Option<u32>,
    pub field: IssueField,
    pub cause: IssueCause,
}

impl ScanIssue {
    pub fn sort_key(&self) -> (bool, u32, &'static str, &'static str) {
        (
            self.pid.is_some(),
            self.pid.unwrap_or(0),
            self.field.as_str(),
            self.cause.as_str(),
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PresenceState {
    Present,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Presence {
    pub state: PresenceState,
    pub cause: Option<IssueCause>,
}

impl Presence {
    pub fn present() -> Self {
        Self {
            state: PresenceState::Present,
            cause: None,
        }
    }

    pub fn unknown(cause: IssueCause) -> Self {
        Self {
            state: PresenceState::Unknown,
            cause: Some(cause),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CwdState {
    Known,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Cwd {
    pub state: CwdState,
    pub value: Option<String>,
    pub cause: Option<IssueCause>,
}

impl Cwd {
    pub fn known(value: String) -> Self {
        Self {
            state: CwdState::Known,
            value: Some(value),
            cause: None,
        }
    }

    pub fn unknown(cause: IssueCause) -> Self {
        Self {
            state: CwdState::Unknown,
            value: None,
            cause: Some(cause),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ActivityState {
    Active,
    Idle,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ActivitySource {
    Hook,
    None,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Activity {
    pub state: ActivityState,
    pub source: ActivitySource,
    pub observed_at_unix_ms: Option<u64>,
}

impl Activity {
    pub fn unknown() -> Self {
        Self {
            state: ActivityState::Unknown,
            source: ActivitySource::None,
            observed_at_unix_ms: None,
        }
    }

    pub fn hook(state: ActivityState, observed_at_unix_ms: u64) -> Self {
        Self {
            state,
            source: ActivitySource::Hook,
            observed_at_unix_ms: Some(observed_at_unix_ms),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AgentRecord {
    pub id: AgentId,
    pub harness: Harness,
    pub detected_by: DetectedBy,
    pub presence: Presence,
    pub cwd: Cwd,
    pub activity: Activity,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectedBy {
    ProcComm,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ScanState {
    Complete,
    Degraded,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scan {
    pub state: ScanState,
    pub issues: Vec<ScanIssue>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotReason {
    Initial,
    RosterChanged,
    ActivityChanged,
    ScanChanged,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Snapshot {
    #[serde(rename = "type")]
    pub frame_type: SnapshotType,
    pub reason: SnapshotReason,
    pub schema: SnapshotSchema,
    pub instance_id: String,
    pub revision: u64,
    pub observed_at_unix_ms: u64,
    pub scan: Scan,
    pub agents: Vec<AgentRecord>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SnapshotType {
    Snapshot,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SnapshotSchema {
    #[serde(rename = "agentd.snapshot.v1")]
    V1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanProposal {
    pub observed_at_unix_ms: u64,
    pub scan: Scan,
    pub agents: Vec<AgentRecord>,
}

impl AgentRecord {
    pub fn sort_key(&self) -> (&'static str, u32, u64) {
        (self.harness.as_str(), self.id.pid, self.id.start_time_ticks)
    }
}
