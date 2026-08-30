use crate::Clock;
use crate::model::{
    Activity, AgentId, AgentRecord, Cwd, DetectedBy, Harness, IssueCause, IssueField, Presence,
    ProcessLiveness, Scan, ScanIssue, ScanProposal, ScanState, Snapshot,
};
use crate::tmux::{NoTmux, SystemTmux, TmuxSource};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone)]
pub struct ProcfsScanner {
    view: Arc<dyn ProcfsView>,
    effective_uid: u32,
    tmux: Arc<dyn TmuxSource>,
}

pub trait ProcfsView: Send + Sync {
    fn enumerate_pids(&self) -> io::Result<Vec<u32>>;
    fn read_stat(&self, pid: u32) -> io::Result<String>;
    fn read_status(&self, pid: u32) -> io::Result<String>;
    fn read_cwd(&self, pid: u32) -> io::Result<PathBuf>;
    fn read_system_stat(&self) -> io::Result<String> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "system procfs stat unavailable",
        ))
    }
    fn ticks_per_second(&self) -> io::Result<u64> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "clock tick rate unavailable",
        ))
    }
}

#[derive(Clone, Debug)]
struct FilesystemProcfs {
    root: PathBuf,
}

impl ProcfsView for FilesystemProcfs {
    fn enumerate_pids(&self) -> io::Result<Vec<u32>> {
        enumerate_pids(&self.root)
    }

    fn read_stat(&self, pid: u32) -> io::Result<String> {
        fs::read_to_string(self.root.join(pid.to_string()).join("stat"))
    }

    fn read_status(&self, pid: u32) -> io::Result<String> {
        fs::read_to_string(self.root.join(pid.to_string()).join("status"))
    }

    fn read_cwd(&self, pid: u32) -> io::Result<PathBuf> {
        fs::read_link(self.root.join(pid.to_string()).join("cwd"))
    }

    fn read_system_stat(&self) -> io::Result<String> {
        fs::read_to_string(self.root.join("stat"))
    }

    fn ticks_per_second(&self) -> io::Result<u64> {
        let value = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        u64::try_from(value)
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| io::Error::other("invalid clock tick rate"))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatRecord {
    pub pid: u32,
    pub parent_pid: u32,
    pub comm: String,
    pub state: char,
    pub tty_nr: i32,
    pub start_time_ticks: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RootResolution {
    Root,
    Child,
    Raced,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HookResolutionError {
    AncestryUnresolved,
    ProcessIdentityChanged,
}

impl ProcfsScanner {
    pub fn system() -> Self {
        Self {
            view: Arc::new(FilesystemProcfs {
                root: PathBuf::from("/proc"),
            }),
            effective_uid: unsafe { libc::geteuid() as u32 },
            tmux: Arc::new(SystemTmux),
        }
    }

    pub fn new(root: PathBuf, effective_uid: u32) -> Self {
        Self::from_view(Arc::new(FilesystemProcfs { root }), effective_uid)
    }

    pub fn from_view(view: Arc<dyn ProcfsView>, effective_uid: u32) -> Self {
        Self {
            view,
            effective_uid,
            tmux: Arc::new(NoTmux),
        }
    }

    #[cfg(test)]
    pub fn from_view_with_tmux(
        view: Arc<dyn ProcfsView>,
        effective_uid: u32,
        tmux: Arc<dyn TmuxSource>,
    ) -> Self {
        Self {
            view,
            effective_uid,
            tmux,
        }
    }

    pub fn resolve_hook_root(
        &self,
        start_pid: u32,
        harness: Harness,
    ) -> Result<AgentId, HookResolutionError> {
        let mut visited = HashSet::new();
        let mut pid = start_pid;
        let mut matching_candidates = Vec::new();

        while pid != 0 {
            if !visited.insert(pid) {
                return Err(HookResolutionError::AncestryUnresolved);
            }
            let first = self
                .read_stat(pid)
                .map_err(|_| HookResolutionError::AncestryUnresolved)?;
            let parent_pid = first.parent_pid;

            if let Some(candidate_harness) = live_harness(&first) {
                let effective_uid = self
                    .read_effective_uid(pid)
                    .map_err(|_| HookResolutionError::AncestryUnresolved)?;
                if effective_uid == self.effective_uid {
                    let second = self
                        .read_stat(pid)
                        .map_err(|_| HookResolutionError::ProcessIdentityChanged)?;
                    if second.start_time_ticks != first.start_time_ticks
                        || live_harness(&second) != Some(candidate_harness)
                    {
                        return Err(HookResolutionError::ProcessIdentityChanged);
                    }
                    if second.parent_pid != first.parent_pid {
                        return Err(HookResolutionError::AncestryUnresolved);
                    }
                    if candidate_harness == harness {
                        matching_candidates.push(second);
                    }
                }
            }
            pid = parent_pid;
        }

        let root = matching_candidates
            .last()
            .ok_or(HookResolutionError::AncestryUnresolved)?;
        let final_read = self
            .read_stat(root.pid)
            .map_err(|_| HookResolutionError::ProcessIdentityChanged)?;
        if final_read.start_time_ticks != root.start_time_ticks
            || live_harness(&final_read) != Some(harness)
        {
            return Err(HookResolutionError::ProcessIdentityChanged);
        }
        Ok(AgentId {
            pid: root.pid,
            start_time_ticks: root.start_time_ticks,
        })
    }

    pub fn scan(&self, previous: Option<&Snapshot>, clock: &dyn Clock) -> ScanProposal {
        let observed_at_unix_ms = clock.now_unix_ms();
        let previous_agents = previous.map_or(&[][..], |snapshot| snapshot.agents.as_slice());
        let previous_by_id: HashMap<AgentId, &AgentRecord> = previous_agents
            .iter()
            .map(|agent| (agent.id, agent))
            .collect();
        let mut issues = Vec::new();
        let pids = match self.view.enumerate_pids() {
            Ok(pids) => pids,
            Err(_) => {
                let agents = previous_agents
                    .iter()
                    .map(|agent| {
                        let mut retained = agent.clone();
                        retained.presence = Presence::unknown(IssueCause::ProcUnavailable);
                        retained
                    })
                    .collect();
                return ScanProposal {
                    observed_at_unix_ms,
                    scan: Scan {
                        state: ScanState::Degraded,
                        issues: vec![ScanIssue {
                            pid: None,
                            field: IssueField::Proc,
                            cause: IssueCause::ProcUnavailable,
                        }],
                    },
                    agents,
                    process_liveness: None,
                };
            }
        };

        let tmux_index = self.tmux.index();
        let started_at_inputs = self
            .view
            .read_system_stat()
            .and_then(|content| parse_boot_time(&content))
            .and_then(|boot_time| {
                self.view
                    .ticks_per_second()
                    .map(|ticks_per_second| (boot_time, ticks_per_second))
            })
            .ok();
        let mut enumerated_pids: BTreeSet<u32> = pids.iter().copied().collect();

        let mut first_reads = BTreeMap::new();
        let mut unknown = HashMap::<AgentId, IssueCause>::new();
        for pid in pids {
            match self.read_stat(pid) {
                Ok(stat) => {
                    first_reads.insert(pid, stat);
                }
                Err(error) => {
                    let cause = read_cause(&error);
                    issues.push(issue(pid, IssueField::Stat, cause));
                    if error.kind() == io::ErrorKind::NotFound {
                        enumerated_pids.remove(&pid);
                    } else {
                        for old in previous_agents.iter().filter(|agent| agent.id.pid == pid) {
                            unknown.entry(old.id).or_insert(cause);
                        }
                    }
                }
            }
        }

        let mut validated = BTreeMap::<u32, (StatRecord, Harness, Option<String>)>::new();
        for first in first_reads.values() {
            let Some(harness) = live_harness(first) else {
                continue;
            };
            let id = AgentId {
                pid: first.pid,
                start_time_ticks: first.start_time_ticks,
            };

            let effective_uid = match self.read_effective_uid(first.pid) {
                Ok(uid) => uid,
                Err(error) => {
                    let cause = read_cause(&error);
                    issues.push(issue(first.pid, IssueField::Status, cause));
                    if error.kind() != io::ErrorKind::NotFound && previous_by_id.contains_key(&id) {
                        unknown.entry(id).or_insert(cause);
                    }
                    continue;
                }
            };
            if effective_uid != self.effective_uid {
                continue;
            }

            let second = match self.read_stat(first.pid) {
                Ok(stat) => stat,
                Err(error) => {
                    let cause = read_cause(&error);
                    issues.push(issue(first.pid, IssueField::Stat, cause));
                    if error.kind() == io::ErrorKind::NotFound {
                        enumerated_pids.remove(&first.pid);
                    } else if previous_by_id.contains_key(&id) {
                        unknown.entry(id).or_insert(cause);
                    }
                    continue;
                }
            };
            if second.start_time_ticks != first.start_time_ticks
                || live_harness(&second) != Some(harness)
            {
                continue;
            }
            if second.parent_pid != first.parent_pid {
                issues.push(issue(
                    first.pid,
                    IssueField::ParentChain,
                    IssueCause::ProcessRaced,
                ));
                if previous_by_id.contains_key(&id) {
                    unknown.entry(id).or_insert(IssueCause::ProcessRaced);
                }
                continue;
            }
            let tty = (second.tty_nr == first.tty_nr)
                .then(|| canonical_tty(second.tty_nr))
                .flatten();
            validated.insert(first.pid, (second, harness, tty));
        }

        let mut agents = Vec::new();
        for (pid, (candidate, harness, tty)) in &validated {
            let id = AgentId {
                pid: *pid,
                start_time_ticks: candidate.start_time_ticks,
            };
            match resolve_root(candidate, *harness, &first_reads, &validated) {
                RootResolution::Child => {}
                RootResolution::Raced => {
                    issues.push(issue(
                        *pid,
                        IssueField::ParentChain,
                        IssueCause::ProcessRaced,
                    ));
                    if previous_by_id.contains_key(&id) {
                        unknown.entry(id).or_insert(IssueCause::ProcessRaced);
                    }
                }
                RootResolution::Root => {
                    let cwd = match self.view.read_cwd(*pid) {
                        Ok(path) if path.is_absolute() => {
                            Cwd::known(path.to_string_lossy().into_owned())
                        }
                        Ok(_) => {
                            issues.push(issue(*pid, IssueField::Cwd, IssueCause::IoError));
                            Cwd::unknown(IssueCause::IoError)
                        }
                        Err(error) => {
                            let cause = cwd_cause(&error);
                            issues.push(issue(*pid, IssueField::Cwd, cause));
                            Cwd::unknown(cause)
                        }
                    };
                    agents.push(AgentRecord {
                        id,
                        harness: *harness,
                        detected_by: DetectedBy::ProcComm,
                        presence: Presence::present(),
                        cwd,
                        activity: Activity::unknown(),
                        tty: tty.clone(),
                        tmux: tty
                            .as_ref()
                            .and_then(|canonical| tmux_index.get(canonical))
                            .cloned(),
                        name: None,
                        started_at_unix_ms: started_at_inputs.and_then(
                            |(boot_time, ticks_per_second)| {
                                started_at_unix_ms(
                                    boot_time,
                                    candidate.start_time_ticks,
                                    ticks_per_second,
                                )
                            },
                        ),
                    });
                }
            }
        }

        for (id, cause) in unknown {
            if agents.iter().any(|agent| agent.id == id) {
                continue;
            }
            if let Some(old) = previous_by_id.get(&id) {
                let mut retained = (*old).clone();
                retained.presence = Presence::unknown(cause);
                agents.push(retained);
            }
        }

        agents.sort_by_key(AgentRecord::sort_key);
        issues.sort_by_key(ScanIssue::sort_key);
        issues.dedup();
        let degraded = agents
            .iter()
            .any(|agent| agent.presence.state == crate::model::PresenceState::Unknown);
        ScanProposal {
            observed_at_unix_ms,
            scan: Scan {
                state: if degraded {
                    ScanState::Degraded
                } else {
                    ScanState::Complete
                },
                issues,
            },
            agents,
            process_liveness: Some(ProcessLiveness {
                enumerated_pids,
                start_time_ticks: first_reads
                    .values()
                    .map(|stat| (stat.pid, stat.start_time_ticks))
                    .collect(),
            }),
        }
    }

    fn read_stat(&self, pid: u32) -> io::Result<StatRecord> {
        let content = self.view.read_stat(pid)?;
        parse_stat(&content, pid)
    }

    fn read_effective_uid(&self, pid: u32) -> io::Result<u32> {
        let content = self.view.read_status(pid)?;
        parse_effective_uid(&content)
    }
}

pub fn parse_stat(content: &str, expected_pid: u32) -> io::Result<StatRecord> {
    let open = content.find('(').ok_or_else(invalid_data)?;
    let close = content.rfind(')').ok_or_else(invalid_data)?;
    if close <= open {
        return Err(invalid_data());
    }
    let parsed_pid: u32 = content[..open].trim().parse().map_err(|_| invalid_data())?;
    if parsed_pid != expected_pid || parsed_pid == 0 {
        return Err(invalid_data());
    }
    let comm = content[open + 1..close].to_owned();
    let fields: Vec<&str> = content[close + 1..].split_whitespace().collect();
    if fields.len() < 20 {
        return Err(invalid_data());
    }
    let mut state_chars = fields[0].chars();
    let state = state_chars.next().ok_or_else(invalid_data)?;
    if state_chars.next().is_some() {
        return Err(invalid_data());
    }
    let parent_pid = fields[1].parse().map_err(|_| invalid_data())?;
    let tty_nr = fields[4].parse().map_err(|_| invalid_data())?;
    let start_time_ticks = fields[19].parse().map_err(|_| invalid_data())?;
    if start_time_ticks == 0 {
        return Err(invalid_data());
    }
    Ok(StatRecord {
        pid: parsed_pid,
        parent_pid,
        comm,
        state,
        tty_nr,
        start_time_ticks,
    })
}

pub fn parse_effective_uid(content: &str) -> io::Result<u32> {
    let row = content
        .lines()
        .find(|line| line.starts_with("Uid:"))
        .ok_or_else(invalid_data)?;
    let values: Vec<&str> = row[4..].split_whitespace().collect();
    if values.len() != 4 {
        return Err(invalid_data());
    }
    let parsed: Vec<u32> = values
        .iter()
        .map(|value| value.parse().map_err(|_| invalid_data()))
        .collect::<io::Result<_>>()?;
    Ok(parsed[1])
}

fn enumerate_pids(root: &Path) -> io::Result<Vec<u32>> {
    let mut pids = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if let Ok(pid) = name.parse::<u32>()
            && pid > 0
        {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    pids.dedup();
    Ok(pids)
}

fn live_harness(stat: &StatRecord) -> Option<Harness> {
    if stat.state == 'Z' {
        None
    } else {
        Harness::from_comm(&stat.comm)
    }
}

fn resolve_root(
    candidate: &StatRecord,
    harness: Harness,
    first_reads: &BTreeMap<u32, StatRecord>,
    validated: &BTreeMap<u32, (StatRecord, Harness, Option<String>)>,
) -> RootResolution {
    let mut visited = HashSet::from([candidate.pid]);
    let mut parent_pid = candidate.parent_pid;
    while parent_pid != 0 {
        if !visited.insert(parent_pid) {
            return RootResolution::Raced;
        }
        let Some(parent) = first_reads.get(&parent_pid) else {
            return RootResolution::Raced;
        };
        if validated
            .get(&parent_pid)
            .is_some_and(|(_, parent_harness, _)| *parent_harness == harness)
        {
            return RootResolution::Child;
        }
        parent_pid = parent.parent_pid;
    }
    RootResolution::Root
}

pub fn canonical_tty(tty_nr: i32) -> Option<String> {
    if tty_nr == 0 {
        return None;
    }
    let bits = tty_nr as u32;
    let major = (bits >> 8) & 0x0000_0fff;
    let minor = (bits & 0x0000_00ff) | ((bits >> 12) & 0x000f_ff00);
    if (136..=143).contains(&major) {
        let index = (major - 136).checked_mul(256)?.checked_add(minor)?;
        Some(format!("pts/{index}"))
    } else {
        Some(format!("dev/{major}:{minor}"))
    }
}

pub fn parse_boot_time(content: &str) -> io::Result<u64> {
    let mut values = content.lines().filter_map(|line| {
        let mut fields = line.split_whitespace();
        (fields.next() == Some("btime")).then(|| (fields.next(), fields.next()))
    });
    let (Some(value), None) = values.next().ok_or_else(invalid_data)? else {
        return Err(invalid_data());
    };
    if values.next().is_some() {
        return Err(invalid_data());
    }
    value.parse().map_err(|_| invalid_data())
}

pub fn started_at_unix_ms(
    boot_time_unix_seconds: u64,
    start_time_ticks: u64,
    ticks_per_second: u64,
) -> Option<u64> {
    if ticks_per_second == 0 {
        return None;
    }
    let result = (boot_time_unix_seconds as u128)
        .checked_mul(1000)?
        .checked_add(
            (start_time_ticks as u128)
                .checked_mul(1000)?
                .checked_div(ticks_per_second as u128)?,
        )?;
    u64::try_from(result).ok()
}

fn issue(pid: u32, field: IssueField, cause: IssueCause) -> ScanIssue {
    ScanIssue {
        pid: Some(pid),
        field,
        cause,
    }
}

fn read_cause(error: &io::Error) -> IssueCause {
    match error.kind() {
        io::ErrorKind::PermissionDenied => IssueCause::PermissionDenied,
        io::ErrorKind::NotFound => IssueCause::ProcessRaced,
        _ => IssueCause::IoError,
    }
}

fn cwd_cause(error: &io::Error) -> IssueCause {
    read_cause(error)
}

fn invalid_data() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid procfs record")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ActivityState, CwdState, PresenceState, ScanState};
    use crate::state::StateStore;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FixedClock(u64);

    impl Clock for FixedClock {
        fn now_unix_ms(&self) -> u64 {
            self.0
        }
    }

    struct TestProcfs(PathBuf);

    impl TestProcfs {
        fn new(label: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "agentd-procfs-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir(&root).unwrap();
            fs::create_dir(root.join("cwd-target")).unwrap();
            Self(root)
        }

        fn scanner(&self) -> ProcfsScanner {
            ProcfsScanner::new(self.0.clone(), 1000)
        }

        fn write_process(
            &self,
            pid: u32,
            comm: &str,
            parent_pid: u32,
            start_time_ticks: u64,
            effective_uid: u32,
        ) {
            self.write_process_with_tty(pid, comm, parent_pid, start_time_ticks, effective_uid, 0);
        }

        fn write_process_with_tty(
            &self,
            pid: u32,
            comm: &str,
            parent_pid: u32,
            start_time_ticks: u64,
            effective_uid: u32,
            tty_nr: i32,
        ) {
            let directory = self.0.join(pid.to_string());
            fs::create_dir_all(&directory).unwrap();
            fs::write(
                directory.join("stat"),
                synthetic_stat_with_tty(pid, comm, parent_pid, start_time_ticks, tty_nr),
            )
            .unwrap();
            fs::write(
                directory.join("status"),
                format!("Name:\t{comm}\nUid:\t{effective_uid}\t{effective_uid}\t{effective_uid}\t{effective_uid}\n"),
            )
            .unwrap();
            let cwd = directory.join("cwd");
            if fs::symlink_metadata(&cwd).is_ok() {
                fs::remove_file(&cwd).unwrap();
            }
            symlink(self.0.join("cwd-target"), cwd).unwrap();
        }
    }

    impl Drop for TestProcfs {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn parses_comm_with_spaces_and_right_parenthesis() {
        let stat = "42 (name with ) paren) S 7 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 99 0";
        let parsed = parse_stat(stat, 42).unwrap();
        assert_eq!(parsed.comm, "name with ) paren");
        assert_eq!(parsed.parent_pid, 7);
        assert_eq!(parsed.state, 'S');
        assert_eq!(parsed.tty_nr, 0);
        assert_eq!(parsed.start_time_ticks, 99);
    }

    #[test]
    fn tty_and_started_at_use_exact_linux_integer_rules() {
        let stat = synthetic_stat_with_tty(42, "name with ) paren", 7, 250, 34_829);
        let parsed = parse_stat(&stat, 42).unwrap();
        assert_eq!(canonical_tty(parsed.tty_nr).as_deref(), Some("pts/13"));
        assert_eq!(
            started_at_unix_ms(1_700_000_000, parsed.start_time_ticks, 100),
            Some(1_700_000_002_500)
        );
        assert_eq!(started_at_unix_ms(u64::MAX, 1, 100), None);
        assert_eq!(started_at_unix_ms(1, 1, 0), None);
        assert_eq!(
            parse_boot_time("cpu 1\nbtime 1700000000\n").unwrap(),
            1_700_000_000
        );
        assert!(parse_boot_time("btime 1\nbtime 2\n").is_err());
    }

    #[test]
    fn scanner_builds_one_tmux_index_and_maps_only_matching_tty() {
        struct CountingTmux {
            calls: AtomicUsize,
        }
        impl TmuxSource for CountingTmux {
            fn index(&self) -> crate::tmux::TmuxIndex {
                self.calls.fetch_add(1, Ordering::SeqCst);
                HashMap::from([(
                    "pts/13".into(),
                    crate::model::TmuxLocation {
                        session: "agents".into(),
                        window_index: 2,
                        window_name: "spec".into(),
                        pane_id: "%7".into(),
                    },
                )])
            }
        }

        let procfs = TestProcfs::new("tmux-index");
        procfs.write_process_with_tty(10, "codex", 0, 100, 1000, 34_829);
        procfs.write_process(20, "claude", 0, 200, 1000);
        let tmux = Arc::new(CountingTmux {
            calls: AtomicUsize::new(0),
        });
        let scanner = ProcfsScanner::from_view_with_tmux(
            Arc::new(FilesystemProcfs {
                root: procfs.0.clone(),
            }),
            1000,
            tmux.clone(),
        );
        let proposal = scanner.scan(None, &FixedClock(1));
        assert_eq!(tmux.calls.load(Ordering::SeqCst), 1);
        let codex = proposal
            .agents
            .iter()
            .find(|agent| agent.id.pid == 10)
            .unwrap();
        assert_eq!(codex.tty.as_deref(), Some("pts/13"));
        assert_eq!(codex.tmux.as_ref().unwrap().pane_id, "%7");
        let claude = proposal
            .agents
            .iter()
            .find(|agent| agent.id.pid == 20)
            .unwrap();
        assert_eq!(claude.tty, None);
        assert_eq!(claude.tmux, None);
    }

    #[test]
    fn tty_change_between_identity_reads_is_null_without_losing_presence() {
        let view = Arc::new(TtyRaceView {
            reads: AtomicUsize::new(0),
        });
        let proposal = ProcfsScanner::from_view(view, 1000).scan(None, &FixedClock(1));
        assert_eq!(proposal.agents.len(), 1);
        assert_eq!(proposal.agents[0].presence.state, PresenceState::Present);
        assert_eq!(proposal.agents[0].tty, None);
        assert_eq!(proposal.agents[0].tmux, None);
    }

    #[test]
    fn effective_uid_is_second_value() {
        let status = "Name:\tcodex\nUid:\t1000\t1001\t1002\t1003\n";
        assert_eq!(parse_effective_uid(status).unwrap(), 1001);
    }

    #[test]
    fn rejects_zero_start_time() {
        let stat = "42 (codex) S 7 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0";
        assert!(parse_stat(stat, 42).is_err());
    }

    #[test]
    fn scanner_emits_roots_collapses_helpers_and_excludes_foreign_uid() {
        let procfs = TestProcfs::new("roots");
        procfs.write_process(10, "codex", 0, 100, 1000);
        procfs.write_process(11, "codex", 10, 110, 1000);
        procfs.write_process(12, "codex", 0, 120, 2000);
        procfs.write_process(13, "claude.exe", 0, 130, 1000);
        procfs.write_process(14, "codex", 0, 140, 2000);
        procfs.write_process(15, "codex", 14, 150, 1000);
        let proposal = procfs.scanner().scan(None, &FixedClock(1));
        assert_eq!(proposal.scan.state, ScanState::Complete);
        assert_eq!(
            proposal
                .agents
                .iter()
                .map(|agent| agent.id.pid)
                .collect::<Vec<_>>(),
            vec![13, 10, 15]
        );
    }

    #[test]
    fn hook_resolver_maps_nested_same_harness_to_highest_root() {
        let procfs = TestProcfs::new("hook-root");
        procfs.write_process(410, "claude", 0, 4_100, 1000);
        procfs.write_process(420, "bash", 410, 4_200, 1000);
        procfs.write_process(430, "claude", 420, 4_300, 1000);
        procfs.write_process(440, "bash", 430, 4_400, 1000);
        assert_eq!(
            procfs.scanner().resolve_hook_root(440, Harness::Claude),
            Ok(AgentId {
                pid: 410,
                start_time_ticks: 4_100,
            })
        );
    }

    #[test]
    fn hook_resolver_crosses_different_harness_candidates() {
        let procfs = TestProcfs::new("hook-cross-harness");
        procfs.write_process(510, "codex", 0, 5_100, 1000);
        procfs.write_process(520, "claude", 510, 5_200, 1000);
        procfs.write_process(530, "codex", 520, 5_300, 1000);
        procfs.write_process(540, "bash", 530, 5_400, 1000);
        assert_eq!(
            procfs.scanner().resolve_hook_root(540, Harness::Codex),
            Ok(AgentId {
                pid: 510,
                start_time_ticks: 5_100,
            })
        );
        assert_eq!(
            procfs.scanner().resolve_hook_root(540, Harness::Claude),
            Ok(AgentId {
                pid: 520,
                start_time_ticks: 5_200,
            })
        );
    }

    #[test]
    fn root_promotion_discards_activity_before_child_returns() {
        let procfs = TestProcfs::new("promotion");
        procfs.write_process(20, "bash", 0, 200, 1000);
        procfs.write_process(21, "codex", 20, 210, 1000);
        let scanner = procfs.scanner();
        let store = StateStore::with_instance_id("0123456789abcdef0123456789abcdef".into());
        store.commit_scan(scanner.scan(None, &FixedClock(1)));
        store
            .apply_activity(
                AgentId {
                    pid: 21,
                    start_time_ticks: 210,
                },
                ActivityState::Active,
                &FixedClock(2),
            )
            .unwrap();

        procfs.write_process(20, "codex", 0, 200, 1000);
        let promoted = scanner.scan(store.current_snapshot().as_deref(), &FixedClock(3));
        let promoted = store.commit_scan(promoted);
        assert_eq!(promoted.agents.len(), 1);
        assert_eq!(promoted.agents[0].id.pid, 20);
        assert_eq!(promoted.agents[0].activity.state, ActivityState::Unknown);

        procfs.write_process(20, "bash", 0, 200, 1000);
        let returned = scanner.scan(store.current_snapshot().as_deref(), &FixedClock(4));
        let returned = store.commit_scan(returned);
        assert_eq!(returned.agents.len(), 1);
        assert_eq!(returned.agents[0].id.pid, 21);
        assert_eq!(returned.agents[0].activity.state, ActivityState::Unknown);
    }

    #[test]
    fn malformed_retained_status_is_unknown_and_degraded() {
        let procfs = TestProcfs::new("unknown-status");
        procfs.write_process(30, "codex", 0, 300, 1000);
        let scanner = procfs.scanner();
        let first = scanner.scan(None, &FixedClock(1));
        let store = StateStore::with_instance_id("0123456789abcdef0123456789abcdef".into());
        store.commit_scan(first);
        fs::write(procfs.0.join("30/status"), "invalid status\n").unwrap();
        let next = scanner.scan(store.current_snapshot().as_deref(), &FixedClock(2));
        assert_eq!(next.scan.state, ScanState::Degraded);
        assert_eq!(next.agents[0].presence.state, PresenceState::Unknown);
        assert_eq!(next.agents[0].presence.cause, Some(IssueCause::IoError));
    }

    #[test]
    fn cwd_failure_is_explicit_without_degrading_scan() {
        let procfs = TestProcfs::new("unknown-cwd");
        procfs.write_process(40, "claude", 0, 400, 1000);
        fs::remove_file(procfs.0.join("40/cwd")).unwrap();
        let proposal = procfs.scanner().scan(None, &FixedClock(1));
        assert_eq!(proposal.scan.state, ScanState::Complete);
        assert_eq!(proposal.agents[0].cwd.state, CwdState::Unknown);
        assert_eq!(proposal.agents[0].cwd.value, None);
        assert_eq!(proposal.agents[0].cwd.cause, Some(IssueCause::ProcessRaced));
    }

    #[test]
    fn enumeration_failure_preserves_old_identity_as_unknown() {
        let procfs = TestProcfs::new("enumeration");
        procfs.write_process(50, "codex", 0, 500, 1000);
        let scanner = procfs.scanner();
        let first = scanner.scan(None, &FixedClock(1));
        fs::remove_dir_all(&procfs.0).unwrap();
        let failed = scanner.scan(
            Some(&Snapshot {
                frame_type: crate::model::SnapshotType::Snapshot,
                reason: crate::model::SnapshotReason::Initial,
                schema: crate::model::SnapshotSchema::V1,
                instance_id: "0123456789abcdef0123456789abcdef".into(),
                revision: 1,
                observed_at_unix_ms: 1,
                scan: first.scan,
                agents: first.agents,
            }),
            &FixedClock(2),
        );
        assert_eq!(failed.scan.state, ScanState::Degraded);
        assert_eq!(failed.scan.issues[0].pid, None);
        assert_eq!(failed.agents[0].presence.state, PresenceState::Unknown);
        assert_eq!(
            failed.agents[0].presence.cause,
            Some(IssueCause::ProcUnavailable)
        );
    }

    #[test]
    fn parent_cycle_never_invents_new_root_and_retains_old_as_unknown() {
        let procfs = TestProcfs::new("cycle");
        procfs.write_process(100, "codex", 0, 1_000, 1000);
        let scanner = procfs.scanner();
        let first = scanner.scan(None, &FixedClock(1));
        let store = StateStore::with_instance_id("0123456789abcdef0123456789abcdef".into());
        store.commit_scan(first);
        procfs.write_process(100, "codex", 200, 1_000, 1000);
        procfs.write_process(200, "bash", 100, 2_000, 1000);
        let cycled = scanner.scan(store.current_snapshot().as_deref(), &FixedClock(2));
        assert_eq!(cycled.agents.len(), 1);
        assert_eq!(cycled.agents[0].presence.state, PresenceState::Unknown);
        assert!(cycled.scan.issues.iter().any(|issue| {
            issue.pid == Some(100)
                && issue.field == IssueField::ParentChain
                && issue.cause == IssueCause::ProcessRaced
        }));
    }

    #[test]
    fn parent_change_between_stat_reads_is_typed_and_never_adds_new_identity() {
        let view = Arc::new(ParentRaceView {
            reads: AtomicUsize::new(0),
        });
        let scanner = ProcfsScanner::from_view(view.clone(), 1000);
        let new_scan = scanner.scan(None, &FixedClock(1));
        assert!(new_scan.agents.is_empty());
        assert!(new_scan.scan.issues.iter().any(|issue| {
            issue.pid == Some(60)
                && issue.field == IssueField::ParentChain
                && issue.cause == IssueCause::ProcessRaced
        }));

        view.reads.store(0, Ordering::SeqCst);
        let previous = Snapshot {
            frame_type: crate::model::SnapshotType::Snapshot,
            reason: crate::model::SnapshotReason::Initial,
            schema: crate::model::SnapshotSchema::V1,
            instance_id: "0123456789abcdef0123456789abcdef".into(),
            revision: 1,
            observed_at_unix_ms: 0,
            scan: Scan {
                state: ScanState::Complete,
                issues: Vec::new(),
            },
            agents: vec![AgentRecord {
                id: AgentId {
                    pid: 60,
                    start_time_ticks: 600,
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
        };
        let retained_scan = scanner.scan(Some(&previous), &FixedClock(2));
        assert_eq!(retained_scan.agents.len(), 1);
        assert_eq!(
            retained_scan.agents[0].presence,
            Presence::unknown(IssueCause::ProcessRaced)
        );
        assert_eq!(retained_scan.scan.state, ScanState::Degraded);
    }

    #[test]
    fn reused_pid_replaces_identity_and_drops_activity_in_one_commit() {
        let procfs = TestProcfs::new("pid-reuse");
        procfs.write_process(70, "codex", 0, 700, 1000);
        let scanner = procfs.scanner();
        let store = StateStore::with_instance_id("0123456789abcdef0123456789abcdef".into());
        store.commit_scan(scanner.scan(None, &FixedClock(1)));
        store
            .apply_activity(
                AgentId {
                    pid: 70,
                    start_time_ticks: 700,
                },
                ActivityState::Active,
                &FixedClock(2),
            )
            .unwrap();
        procfs.write_process(70, "codex", 0, 701, 1000);
        let replacement = scanner.scan(store.current_snapshot().as_deref(), &FixedClock(3));
        let replacement = store.commit_scan(replacement);
        assert_eq!(replacement.agents.len(), 1);
        assert_eq!(replacement.agents[0].id.start_time_ticks, 701);
        assert_eq!(replacement.agents[0].activity.state, ActivityState::Unknown);
        assert_eq!(
            replacement.reason,
            crate::model::SnapshotReason::RosterChanged
        );
    }

    struct ParentRaceView {
        reads: AtomicUsize,
    }

    struct TtyRaceView {
        reads: AtomicUsize,
    }

    impl ProcfsView for TtyRaceView {
        fn enumerate_pids(&self) -> io::Result<Vec<u32>> {
            Ok(vec![61])
        }

        fn read_stat(&self, pid: u32) -> io::Result<String> {
            assert_eq!(pid, 61);
            let tty = if self.reads.fetch_add(1, Ordering::SeqCst).is_multiple_of(2) {
                34_829
            } else {
                34_830
            };
            Ok(synthetic_stat_with_tty(61, "codex", 0, 610, tty))
        }

        fn read_status(&self, pid: u32) -> io::Result<String> {
            assert_eq!(pid, 61);
            Ok("Uid:\t1000\t1000\t1000\t1000\n".into())
        }

        fn read_cwd(&self, _pid: u32) -> io::Result<PathBuf> {
            Ok(PathBuf::from("/work"))
        }
    }

    impl ProcfsView for ParentRaceView {
        fn enumerate_pids(&self) -> io::Result<Vec<u32>> {
            Ok(vec![60])
        }

        fn read_stat(&self, pid: u32) -> io::Result<String> {
            assert_eq!(pid, 60);
            let read = self.reads.fetch_add(1, Ordering::SeqCst);
            let parent = if read.is_multiple_of(2) { 0 } else { 70 };
            Ok(synthetic_stat(60, "codex", parent, 600))
        }

        fn read_status(&self, pid: u32) -> io::Result<String> {
            assert_eq!(pid, 60);
            Ok("Uid:\t1000\t1000\t1000\t1000\n".into())
        }

        fn read_cwd(&self, _pid: u32) -> io::Result<PathBuf> {
            Ok(PathBuf::from("/work"))
        }
    }

    fn synthetic_stat(pid: u32, comm: &str, parent_pid: u32, start: u64) -> String {
        synthetic_stat_with_tty(pid, comm, parent_pid, start, 0)
    }

    fn synthetic_stat_with_tty(
        pid: u32,
        comm: &str,
        parent_pid: u32,
        start: u64,
        tty_nr: i32,
    ) -> String {
        let mut fields = vec!["S".to_owned(), parent_pid.to_string()];
        fields.extend(std::iter::repeat_n("0".to_owned(), 17));
        fields[4] = tty_nr.to_string();
        fields.push(start.to_string());
        format!("{pid} ({comm}) {}\n", fields.join(" "))
    }
}
