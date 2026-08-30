use crate::Clock;
use crate::model::{
    Activity, ActivityState, AgentId, AgentRecord, PresenceState, ProcessLiveness, ScanProposal,
    Snapshot, SnapshotReason, SnapshotSchema, SnapshotType,
};
use crate::names::{NameStore, NameStoreUnavailable};
use serde::Serialize;
use std::fs::File;
use std::io::{self, Read};
use std::sync::{Arc, Condvar, Mutex, Weak};

pub struct SubscriberSlot {
    inner: Mutex<SlotInner>,
    ready: Condvar,
}

#[derive(Default)]
struct SlotInner {
    pending: Option<Arc<Vec<u8>>>,
    closed: bool,
}

impl SubscriberSlot {
    fn new() -> Self {
        Self {
            inner: Mutex::new(SlotInner::default()),
            ready: Condvar::new(),
        }
    }

    pub fn offer(&self, frame: Arc<Vec<u8>>) {
        let mut inner = self.inner.lock().expect("subscriber slot lock poisoned");
        if !inner.closed {
            inner.pending = Some(frame);
            self.ready.notify_one();
        }
    }

    pub fn take(&self) -> Option<Arc<Vec<u8>>> {
        let mut inner = self.inner.lock().expect("subscriber slot lock poisoned");
        while inner.pending.is_none() && !inner.closed {
            inner = self
                .ready
                .wait(inner)
                .expect("subscriber slot lock poisoned while waiting");
        }
        inner.pending.take()
    }

    pub fn close(&self) {
        let mut inner = self.inner.lock().expect("subscriber slot lock poisoned");
        inner.closed = true;
        inner.pending = None;
        self.ready.notify_all();
    }
}

pub struct Subscription {
    pub initial: Arc<Vec<u8>>,
    pub slot: Arc<SubscriberSlot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivityAck {
    pub instance_id: String,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivityError {
    UnknownAgent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NameError {
    UnknownAgent,
    StoreUnavailable,
}

pub struct StateStore {
    instance_id: String,
    inner: Mutex<StateInner>,
}

struct StateInner {
    current: Option<Arc<Snapshot>>,
    encoded: Option<Arc<Vec<u8>>>,
    subscribers: Vec<Weak<SubscriberSlot>>,
    names: NameStore,
    process_liveness: Option<ProcessLiveness>,
}

impl StateStore {
    pub fn new() -> io::Result<Self> {
        Ok(Self::with_name_store(
            random_instance_id()?,
            NameStore::system(),
        ))
    }

    pub fn with_instance_id(instance_id: String) -> Self {
        Self::with_name_store(instance_id, NameStore::memory())
    }

    fn with_name_store(instance_id: String, names: NameStore) -> Self {
        assert!(
            instance_id.len() == 32
                && instance_id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "instance ID must be 32 lowercase hexadecimal digits"
        );
        Self {
            instance_id,
            inner: Mutex::new(StateInner {
                current: None,
                encoded: None,
                subscribers: Vec::new(),
                names,
                process_liveness: None,
            }),
        }
    }

    pub fn current_snapshot(&self) -> Option<Arc<Snapshot>> {
        self.inner
            .lock()
            .expect("state lock poisoned")
            .current
            .clone()
    }

    pub fn snapshot_frame(&self) -> Arc<Vec<u8>> {
        self.inner
            .lock()
            .expect("state lock poisoned")
            .encoded
            .clone()
            .expect("first scan must commit before serving requests")
    }

    pub fn subscribe(&self) -> Subscription {
        let mut inner = self.inner.lock().expect("state lock poisoned");
        let initial = inner
            .encoded
            .clone()
            .expect("first scan must commit before serving requests");
        let slot = Arc::new(SubscriberSlot::new());
        inner.subscribers.push(Arc::downgrade(&slot));
        Subscription { initial, slot }
    }

    pub fn commit_scan(&self, mut proposal: ScanProposal) -> Arc<Snapshot> {
        proposal.agents.sort_by_key(AgentRecord::sort_key);
        proposal
            .scan
            .issues
            .sort_by_key(crate::model::ScanIssue::sort_key);
        proposal.scan.issues.dedup();

        let mut inner = self.inner.lock().expect("state lock poisoned");
        inner.names.cleanup(proposal.process_liveness.as_ref());
        inner.process_liveness = proposal.process_liveness;
        for proposed in &mut proposal.agents {
            proposed.name = inner.names.name_for(proposed.id).map(str::to_owned);
        }
        if let Some(current) = &inner.current {
            for proposed in &mut proposal.agents {
                if let Some(existing) = current.agents.iter().find(|agent| agent.id == proposed.id)
                {
                    proposed.activity = existing.activity.clone();
                }
            }
        }

        let (revision, reason, changed) = match &inner.current {
            None => (1, SnapshotReason::Initial, true),
            Some(current) => {
                let roster_changed =
                    non_activity_agents(&current.agents) != non_activity_agents(&proposal.agents);
                let activity_changed = activities(&current.agents) != activities(&proposal.agents);
                let scan_changed = current.scan != proposal.scan;
                if !(roster_changed || activity_changed || scan_changed) {
                    (current.revision, current.reason, false)
                } else {
                    let reason = if roster_changed {
                        SnapshotReason::RosterChanged
                    } else if activity_changed {
                        SnapshotReason::ActivityChanged
                    } else {
                        SnapshotReason::ScanChanged
                    };
                    (
                        current
                            .revision
                            .checked_add(1)
                            .expect("snapshot revision overflow"),
                        reason,
                        true,
                    )
                }
            }
        };

        let snapshot = Arc::new(Snapshot {
            frame_type: SnapshotType::Snapshot,
            reason,
            schema: SnapshotSchema::V1,
            instance_id: self.instance_id.clone(),
            revision,
            observed_at_unix_ms: proposal.observed_at_unix_ms,
            scan: proposal.scan,
            agents: proposal.agents,
        });
        let encoded = Arc::new(encode_frame(snapshot.as_ref()));
        inner.current = Some(snapshot.clone());
        inner.encoded = Some(encoded.clone());
        if changed && revision > 1 {
            offer_to_subscribers(&mut inner.subscribers, encoded);
        }
        snapshot
    }

    pub fn apply_activity(
        &self,
        id: AgentId,
        state: ActivityState,
        clock: &dyn Clock,
    ) -> Result<ActivityAck, ActivityError> {
        debug_assert!(matches!(
            state,
            ActivityState::Active | ActivityState::Idle | ActivityState::NeedsAttention
        ));
        let mut inner = self.inner.lock().expect("state lock poisoned");
        let current = inner
            .current
            .as_ref()
            .expect("first scan must commit before activity requests");
        let Some(index) = current
            .agents
            .iter()
            .position(|agent| agent.id == id && agent.presence.state == PresenceState::Present)
        else {
            return Err(ActivityError::UnknownAgent);
        };

        let proposed = Activity::hook(state, clock.now_unix_ms());
        if current.agents[index].activity == proposed {
            return Ok(ActivityAck {
                instance_id: self.instance_id.clone(),
                revision: current.revision,
            });
        }

        let mut changed = current.as_ref().clone();
        changed.revision = changed
            .revision
            .checked_add(1)
            .expect("snapshot revision overflow");
        changed.reason = SnapshotReason::ActivityChanged;
        changed.agents[index].activity = proposed;
        let revision = changed.revision;
        let snapshot = Arc::new(changed);
        let encoded = Arc::new(encode_frame(snapshot.as_ref()));
        inner.current = Some(snapshot);
        inner.encoded = Some(encoded.clone());
        offer_to_subscribers(&mut inner.subscribers, encoded);

        Ok(ActivityAck {
            instance_id: self.instance_id.clone(),
            revision,
        })
    }

    pub fn apply_name(&self, id: AgentId, name: Option<String>) -> Result<ActivityAck, NameError> {
        let mut inner = self.inner.lock().expect("state lock poisoned");
        let current = inner
            .current
            .as_ref()
            .expect("first scan must commit before name requests");
        let Some(index) = current
            .agents
            .iter()
            .position(|agent| agent.id == id && agent.presence.state == PresenceState::Present)
        else {
            return Err(NameError::UnknownAgent);
        };
        if current.agents[index].name == name {
            return Ok(ActivityAck {
                instance_id: self.instance_id.clone(),
                revision: current.revision,
            });
        }

        let liveness = inner.process_liveness.clone();
        inner
            .names
            .replace(id, name.clone(), liveness.as_ref())
            .map_err(|NameStoreUnavailable| NameError::StoreUnavailable)?;

        let current = inner
            .current
            .as_ref()
            .expect("first scan must commit before name requests");
        let mut changed = current.as_ref().clone();
        changed.revision = changed
            .revision
            .checked_add(1)
            .expect("snapshot revision overflow");
        changed.reason = SnapshotReason::RosterChanged;
        changed.agents[index].name = name;
        let revision = changed.revision;
        let snapshot = Arc::new(changed);
        let encoded = Arc::new(encode_frame(snapshot.as_ref()));
        inner.current = Some(snapshot);
        inner.encoded = Some(encoded.clone());
        offer_to_subscribers(&mut inner.subscribers, encoded);

        Ok(ActivityAck {
            instance_id: self.instance_id.clone(),
            revision,
        })
    }

    pub fn close_subscribers(&self) {
        let mut inner = self.inner.lock().expect("state lock poisoned");
        inner.subscribers.retain(|weak| {
            if let Some(slot) = weak.upgrade() {
                slot.close();
                true
            } else {
                false
            }
        });
    }
}

#[derive(Eq, PartialEq)]
struct NonActivityAgent<'a> {
    id: AgentId,
    harness: crate::model::Harness,
    detected_by: crate::model::DetectedBy,
    presence: &'a crate::model::Presence,
    cwd: &'a crate::model::Cwd,
    tty: &'a Option<String>,
    tmux: &'a Option<crate::model::TmuxLocation>,
    name: &'a Option<String>,
    started_at_unix_ms: Option<u64>,
}

fn non_activity_agents(agents: &[AgentRecord]) -> Vec<NonActivityAgent<'_>> {
    agents
        .iter()
        .map(|agent| NonActivityAgent {
            id: agent.id,
            harness: agent.harness,
            detected_by: agent.detected_by,
            presence: &agent.presence,
            cwd: &agent.cwd,
            tty: &agent.tty,
            tmux: &agent.tmux,
            name: &agent.name,
            started_at_unix_ms: agent.started_at_unix_ms,
        })
        .collect()
}

fn activities(agents: &[AgentRecord]) -> Vec<(AgentId, &Activity)> {
    agents
        .iter()
        .map(|agent| (agent.id, &agent.activity))
        .collect()
}

fn offer_to_subscribers(subscribers: &mut Vec<Weak<SubscriberSlot>>, frame: Arc<Vec<u8>>) {
    subscribers.retain(|weak| {
        if let Some(slot) = weak.upgrade() {
            slot.offer(frame.clone());
            true
        } else {
            false
        }
    });
}

pub fn encode_frame<T: Serialize>(value: &T) -> Vec<u8> {
    let mut encoded = serde_json::to_vec(value).expect("serializing protocol value cannot fail");
    encoded.push(b'\n');
    encoded
}

fn random_instance_id() -> io::Result<String> {
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let mut output = String::with_capacity(32);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cwd, DetectedBy, Harness, Presence, Scan, ScanState};

    struct FixedClock(u64);

    impl Clock for FixedClock {
        fn now_unix_ms(&self) -> u64 {
            self.0
        }
    }

    fn proposal(observed_at_unix_ms: u64) -> ScanProposal {
        ScanProposal {
            observed_at_unix_ms,
            scan: Scan {
                state: ScanState::Complete,
                issues: Vec::new(),
            },
            agents: vec![AgentRecord {
                id: AgentId {
                    pid: 7,
                    start_time_ticks: 11,
                },
                harness: Harness::Codex,
                detected_by: DetectedBy::ProcComm,
                presence: Presence::present(),
                cwd: Cwd::known("/work".into()),
                activity: Activity::unknown(),
                tty: None,
                tmux: None,
                name: None,
                started_at_unix_ms: None,
            }],
            process_liveness: None,
        }
    }

    #[test]
    fn timestamp_only_scan_retains_revision_and_reason() {
        let store = StateStore::with_instance_id("0123456789abcdef0123456789abcdef".into());
        let first = store.commit_scan(proposal(10));
        let second = store.commit_scan(proposal(20));
        assert_eq!(first.revision, 1);
        assert_eq!(second.revision, 1);
        assert_eq!(second.reason, SnapshotReason::Initial);
        assert_eq!(second.observed_at_unix_ms, 20);
    }

    #[test]
    fn activity_commit_wins_over_scan_started_earlier() {
        let store = StateStore::with_instance_id("0123456789abcdef0123456789abcdef".into());
        store.commit_scan(proposal(10));
        let id = AgentId {
            pid: 7,
            start_time_ticks: 11,
        };
        store
            .apply_activity(id, ActivityState::Active, &FixedClock(30))
            .unwrap();
        let after_scan = store.commit_scan(proposal(40));
        assert_eq!(after_scan.agents[0].activity.state, ActivityState::Active);
        assert_eq!(after_scan.revision, 2);
    }

    #[test]
    fn identical_activity_is_noop_at_equal_clock_time() {
        let store = StateStore::with_instance_id("0123456789abcdef0123456789abcdef".into());
        store.commit_scan(proposal(10));
        let id = AgentId {
            pid: 7,
            start_time_ticks: 11,
        };
        let first = store
            .apply_activity(id, ActivityState::Active, &FixedClock(30))
            .unwrap();
        let second = store
            .apply_activity(id, ActivityState::Active, &FixedClock(30))
            .unwrap();
        assert_eq!(first.revision, 2);
        assert_eq!(second.revision, 2);
    }

    #[test]
    fn name_set_noop_clear_and_stale_identity_use_the_atomic_seam() {
        let store = StateStore::with_instance_id("0123456789abcdef0123456789abcdef".into());
        store.commit_scan(proposal(10));
        let id = AgentId {
            pid: 7,
            start_time_ticks: 11,
        };
        let set = store.apply_name(id, Some("Agentd spec".into())).unwrap();
        assert_eq!(set.revision, 2);
        let snapshot = store.current_snapshot().unwrap();
        assert_eq!(snapshot.reason, SnapshotReason::RosterChanged);
        assert_eq!(snapshot.agents[0].name.as_deref(), Some("Agentd spec"));

        let same = store.apply_name(id, Some("Agentd spec".into())).unwrap();
        assert_eq!(same.revision, 2);
        let clear = store.apply_name(id, None).unwrap();
        assert_eq!(clear.revision, 3);
        assert_eq!(store.current_snapshot().unwrap().agents[0].name, None);
        assert_eq!(store.apply_name(id, None).unwrap().revision, 3);
        assert_eq!(
            store.apply_name(
                AgentId {
                    pid: 7,
                    start_time_ticks: 12,
                },
                Some("wrong".into())
            ),
            Err(NameError::UnknownAgent)
        );
    }

    #[test]
    fn scan_started_before_name_change_cannot_restore_an_old_name() {
        let store = StateStore::with_instance_id("0123456789abcdef0123456789abcdef".into());
        store.commit_scan(proposal(10));
        let stale_proposal = proposal(20);
        store
            .apply_name(
                AgentId {
                    pid: 7,
                    start_time_ticks: 11,
                },
                Some("current".into()),
            )
            .unwrap();
        let after_scan = store.commit_scan(stale_proposal);
        assert_eq!(after_scan.agents[0].name.as_deref(), Some("current"));
        assert_eq!(after_scan.revision, 2);
    }

    #[test]
    fn removed_identity_cannot_be_resurrected_by_activity() {
        let store = StateStore::with_instance_id("0123456789abcdef0123456789abcdef".into());
        store.commit_scan(proposal(10));
        let id = AgentId {
            pid: 7,
            start_time_ticks: 11,
        };
        store.commit_scan(ScanProposal {
            observed_at_unix_ms: 20,
            scan: Scan {
                state: ScanState::Complete,
                issues: Vec::new(),
            },
            agents: Vec::new(),
            process_liveness: None,
        });
        assert_eq!(
            store.apply_activity(id, ActivityState::Active, &FixedClock(30)),
            Err(ActivityError::UnknownAgent)
        );
        assert!(store.current_snapshot().unwrap().agents.is_empty());
    }

    #[test]
    fn scan_issues_sort_by_serialized_tuple_with_null_pid_first() {
        let store = StateStore::with_instance_id("0123456789abcdef0123456789abcdef".into());
        let snapshot = store.commit_scan(ScanProposal {
            observed_at_unix_ms: 1,
            scan: Scan {
                state: ScanState::Complete,
                issues: vec![
                    crate::model::ScanIssue {
                        pid: Some(7),
                        field: crate::model::IssueField::Status,
                        cause: crate::model::IssueCause::PermissionDenied,
                    },
                    crate::model::ScanIssue {
                        pid: None,
                        field: crate::model::IssueField::Proc,
                        cause: crate::model::IssueCause::ProcUnavailable,
                    },
                    crate::model::ScanIssue {
                        pid: Some(7),
                        field: crate::model::IssueField::Cwd,
                        cause: crate::model::IssueCause::ProcessRaced,
                    },
                ],
            },
            agents: Vec::new(),
            process_liveness: None,
        });
        assert_eq!(snapshot.scan.issues[0].pid, None);
        assert_eq!(snapshot.scan.issues[1].field, crate::model::IssueField::Cwd);
        assert_eq!(
            snapshot.scan.issues[2].field,
            crate::model::IssueField::Status
        );
    }

    #[test]
    fn subscriber_slot_coalesces_one_thousand_offers_to_the_latest_frame() {
        let slot = SubscriberSlot::new();
        for revision in 1_u32..=1_000 {
            slot.offer(Arc::new(revision.to_le_bytes().to_vec()));
        }
        assert_eq!(slot.take().unwrap().as_slice(), 1_000_u32.to_le_bytes());
        assert!(slot.inner.lock().unwrap().pending.is_none());
    }
}
