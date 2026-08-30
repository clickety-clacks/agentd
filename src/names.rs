use crate::model::{AgentId, ProcessLiveness};
use crate::protocol::parse_json_value;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NameStoreUnavailable;

pub struct NameStore {
    path: Option<PathBuf>,
    in_memory: bool,
    available: bool,
    effective_uid: u32,
    boot_id: String,
    entries: BTreeMap<AgentId, String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RegistryFile {
    version: u32,
    boot_id: String,
    names: Vec<RegistryEntry>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RegistryEntry {
    agent: AgentId,
    name: String,
}

impl NameStore {
    pub fn system() -> Self {
        let effective_uid = unsafe { libc::geteuid() as u32 };
        let boot_id = read_boot_id().unwrap_or_default();
        let Some(path) = state_file_path() else {
            return Self::unavailable(effective_uid, boot_id, None);
        };
        if boot_id.is_empty() || ensure_safe_directory(&path, effective_uid).is_err() {
            return Self::unavailable(effective_uid, boot_id, Some(path));
        }
        match load_file(&path, effective_uid) {
            Ok(Some(file)) if file.boot_id == boot_id => {
                let mut entries = BTreeMap::new();
                for entry in file.names {
                    if entries.insert(entry.agent, entry.name).is_some() {
                        return Self::unavailable(effective_uid, boot_id, Some(path));
                    }
                }
                Self {
                    path: Some(path),
                    in_memory: false,
                    available: true,
                    effective_uid,
                    boot_id,
                    entries,
                }
            }
            Ok(Some(_)) | Ok(None) => Self {
                path: Some(path),
                in_memory: false,
                available: true,
                effective_uid,
                boot_id,
                entries: BTreeMap::new(),
            },
            Err(_) => Self::unavailable(effective_uid, boot_id, Some(path)),
        }
    }

    pub(crate) fn memory() -> Self {
        Self {
            path: None,
            in_memory: true,
            available: true,
            effective_uid: unsafe { libc::geteuid() as u32 },
            boot_id: "8ddf97c5-8f38-4db7-ae9d-3cc8ac70df44".into(),
            entries: BTreeMap::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_path(path: PathBuf, boot_id: String, effective_uid: u32) -> Self {
        if !valid_boot_id(&boot_id) || ensure_safe_directory(&path, effective_uid).is_err() {
            return Self::unavailable(effective_uid, boot_id, Some(path));
        }
        match load_file(&path, effective_uid) {
            Ok(Some(file)) if file.boot_id == boot_id => Self {
                path: Some(path),
                in_memory: false,
                available: true,
                effective_uid,
                boot_id,
                entries: file
                    .names
                    .into_iter()
                    .map(|entry| (entry.agent, entry.name))
                    .collect(),
            },
            Ok(Some(_)) | Ok(None) => Self {
                path: Some(path),
                in_memory: false,
                available: true,
                effective_uid,
                boot_id,
                entries: BTreeMap::new(),
            },
            Err(_) => Self::unavailable(effective_uid, boot_id, Some(path)),
        }
    }

    fn unavailable(effective_uid: u32, boot_id: String, path: Option<PathBuf>) -> Self {
        Self {
            path,
            in_memory: false,
            available: false,
            effective_uid,
            boot_id,
            entries: BTreeMap::new(),
        }
    }

    pub fn name_for(&self, id: AgentId) -> Option<&str> {
        self.entries.get(&id).map(String::as_str)
    }

    pub fn replace(
        &mut self,
        id: AgentId,
        name: Option<String>,
        liveness: Option<&ProcessLiveness>,
    ) -> Result<(), NameStoreUnavailable> {
        self.recover()?;
        let mut next = pruned_entries(&self.entries, liveness);
        match name {
            Some(name) => {
                next.insert(id, name);
            }
            None => {
                next.remove(&id);
            }
        }
        self.persist(&next)?;
        self.entries = next;
        Ok(())
    }

    pub fn cleanup(&mut self, liveness: Option<&ProcessLiveness>) {
        if !self.available {
            return;
        }
        let next = pruned_entries(&self.entries, liveness);
        if next != self.entries && self.persist(&next).is_ok() {
            self.entries = next;
        }
    }

    fn persist(&self, entries: &BTreeMap<AgentId, String>) -> Result<(), NameStoreUnavailable> {
        if self.in_memory {
            return Ok(());
        }
        if !self.available {
            return Err(NameStoreUnavailable);
        }
        let path = self.path.as_ref().ok_or(NameStoreUnavailable)?;
        ensure_safe_directory(path, self.effective_uid).map_err(|_| NameStoreUnavailable)?;
        validate_existing_file(path, self.effective_uid).map_err(|_| NameStoreUnavailable)?;
        let file = RegistryFile {
            version: 1,
            boot_id: self.boot_id.clone(),
            names: entries
                .iter()
                .map(|(agent, name)| RegistryEntry {
                    agent: *agent,
                    name: name.clone(),
                })
                .collect(),
        };
        let bytes = serde_json::to_vec(&file).map_err(|_| NameStoreUnavailable)?;
        atomic_replace(path, &bytes).map_err(|_| NameStoreUnavailable)
    }

    fn recover(&mut self) -> Result<(), NameStoreUnavailable> {
        if self.available {
            return Ok(());
        }
        let path = self.path.as_ref().ok_or(NameStoreUnavailable)?;
        if !valid_boot_id(&self.boot_id) {
            return Err(NameStoreUnavailable);
        }
        ensure_safe_directory(path, self.effective_uid).map_err(|_| NameStoreUnavailable)?;
        let entries = match load_file(path, self.effective_uid).map_err(|_| NameStoreUnavailable)? {
            Some(file) if file.boot_id == self.boot_id => file
                .names
                .into_iter()
                .map(|entry| (entry.agent, entry.name))
                .collect(),
            Some(_) | None => BTreeMap::new(),
        };
        self.entries = entries;
        self.available = true;
        Ok(())
    }
}

pub fn validate_display_name(name: &str) -> bool {
    (1..=64).contains(&name.len()) && !name.chars().any(char::is_control)
}

fn pruned_entries(
    entries: &BTreeMap<AgentId, String>,
    liveness: Option<&ProcessLiveness>,
) -> BTreeMap<AgentId, String> {
    let Some(liveness) = liveness else {
        return entries.clone();
    };
    entries
        .iter()
        .filter(|(id, _)| {
            liveness.enumerated_pids.contains(&id.pid)
                && liveness
                    .start_time_ticks
                    .get(&id.pid)
                    .is_none_or(|ticks| *ticks == id.start_time_ticks)
        })
        .map(|(id, name)| (*id, name.clone()))
        .collect()
}

fn state_file_path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_STATE_HOME") {
        Some(value) => {
            let path = PathBuf::from(value);
            path.is_absolute().then_some(path)?
        }
        None => {
            let home = PathBuf::from(std::env::var_os("HOME")?);
            home.is_absolute()
                .then_some(home.join(".local").join("state"))?
        }
    };
    Some(base.join("agentd").join("names.json"))
}

fn ensure_safe_directory(path: &Path, effective_uid: u32) -> io::Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::other("missing state directory"))?;
    if let Some(parent) = directory.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::symlink_metadata(directory) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            match builder.create(directory) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(error) => return Err(error),
    }
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != effective_uid
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unsafe Agentd state directory",
        ));
    }
    Ok(())
}

fn validate_existing_file(path: &Path, effective_uid: u32) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.file_type().is_file()
                && !metadata.file_type().is_symlink()
                && metadata.uid() == effective_uid
                && metadata.permissions().mode() & 0o777 == 0o600 =>
        {
            Ok(())
        }
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unsafe Agentd name registry",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn load_file(path: &Path, effective_uid: u32) -> io::Result<Option<RegistryFile>> {
    validate_existing_file(path, effective_uid)?;
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let value = parse_json_value(&bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid name registry JSON"))?;
    let file: RegistryFile = serde_json::from_value(value)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid name registry"))?;
    if file.version != 1 || !valid_boot_id(&file.boot_id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid name registry version or boot ID",
        ));
    }
    let mut identities = HashSet::new();
    if file.names.iter().any(|entry| {
        entry.agent.pid == 0
            || entry.agent.start_time_ticks == 0
            || !validate_display_name(&entry.name)
            || !identities.insert(entry.agent)
    }) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid name registry entry",
        ));
    }
    Ok(Some(file))
}

fn read_boot_id() -> io::Result<String> {
    let content = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    let value = content.strip_suffix('\n').unwrap_or(&content);
    if valid_boot_id(value) {
        Ok(value.to_owned())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid boot ID",
        ))
    }
}

fn valid_boot_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::other("missing state directory"))?;
    let mut temporary = None;
    for attempt in 0..32_u32 {
        let candidate = directory.join(format!(
            ".names.json.tmp.{}.{}.{}",
            std::process::id(),
            monotonic_suffix(),
            attempt
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&candidate)
        {
            Ok(file) => {
                temporary = Some((candidate, file));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    let (temporary_path, mut file) = temporary
        .ok_or_else(|| io::Error::new(io::ErrorKind::AlreadyExists, "temporary file collision"))?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary_path, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

fn monotonic_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn display_name_limits_are_utf8_byte_based_and_reject_controls() {
        assert!(validate_display_name("Agentd spec"));
        assert!(validate_display_name(&"é".repeat(32)));
        assert!(!validate_display_name(""));
        assert!(!validate_display_name(&"é".repeat(33)));
        assert!(!validate_display_name("line\nbreak"));
    }

    #[test]
    fn liveness_prunes_exit_and_pid_reuse_but_not_an_unresolved_stat() {
        let old = AgentId {
            pid: 7,
            start_time_ticks: 11,
        };
        let unresolved = AgentId {
            pid: 8,
            start_time_ticks: 12,
        };
        let entries = BTreeMap::from([(old, "old".into()), (unresolved, "keep".into())]);
        let liveness = ProcessLiveness {
            enumerated_pids: [7, 8].into_iter().collect(),
            start_time_ticks: [(7, 99)].into_iter().collect(),
        };
        assert_eq!(
            pruned_entries(&entries, Some(&liveness)),
            BTreeMap::from([(unresolved, "keep".into())])
        );
    }

    #[test]
    fn safe_registry_round_trips_exact_identity_and_mode() {
        let root = std::env::temp_dir().join(format!(
            "agentd-name-store-{}-{}",
            std::process::id(),
            monotonic_suffix()
        ));
        fs::create_dir(&root).unwrap();
        let path = root.join("agentd").join("names.json");
        let boot_id = "8ddf97c5-8f38-4db7-ae9d-3cc8ac70df44".to_owned();
        let uid = unsafe { libc::geteuid() as u32 };
        let id = AgentId {
            pid: 7,
            start_time_ticks: 11,
        };
        let mut first = NameStore::for_path(path.clone(), boot_id.clone(), uid);
        first.replace(id, Some("Agentd spec".into()), None).unwrap();
        assert_eq!(
            fs::symlink_metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let second = NameStore::for_path(path.clone(), boot_id, uid);
        assert_eq!(second.name_for(id), Some("Agentd spec"));
        let bytes = fs::read(&path).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            serde_json::json!({
                "version": 1,
                "bootId": "8ddf97c5-8f38-4db7-ae9d-3cc8ac70df44",
                "names": [{"agent":{"pid":7,"startTimeTicks":11},"name":"Agentd spec"}]
            })
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_and_symlink_registries_fail_closed_then_recover_after_repair() {
        let root = std::env::temp_dir().join(format!(
            "agentd-name-store-unsafe-{}-{}",
            std::process::id(),
            monotonic_suffix()
        ));
        fs::create_dir(&root).unwrap();
        let directory = root.join("agentd");
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.join("names.json");
        let boot_id = "8ddf97c5-8f38-4db7-ae9d-3cc8ac70df44".to_owned();
        let uid = unsafe { libc::geteuid() as u32 };
        let id = AgentId {
            pid: 7,
            start_time_ticks: 11,
        };

        fs::write(&path, b"{malformed").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let mut malformed = NameStore::for_path(path.clone(), boot_id.clone(), uid);
        assert_eq!(
            malformed.replace(id, Some("private".into()), None),
            Err(NameStoreUnavailable)
        );
        fs::remove_file(&path).unwrap();
        malformed
            .replace(id, Some("recovered".into()), None)
            .unwrap();
        assert_eq!(malformed.name_for(id), Some("recovered"));

        fs::remove_file(&path).unwrap();
        let referent = root.join("referent");
        fs::write(&referent, b"do not follow").unwrap();
        symlink(&referent, &path).unwrap();
        let mut unsafe_store = NameStore::for_path(path.clone(), boot_id, uid);
        assert_eq!(
            unsafe_store.replace(id, Some("private".into()), None),
            Err(NameStoreUnavailable)
        );
        assert_eq!(fs::read(&referent).unwrap(), b"do not follow");
        fs::remove_file(&path).unwrap();
        unsafe_store.replace(id, Some("safe".into()), None).unwrap();
        assert_eq!(fs::read(&referent).unwrap(), b"do not follow");
        fs::remove_dir_all(root).unwrap();
    }
}
