use agentd::model::Harness;
use agentd::procfs::{parse_effective_uid, parse_stat};
use std::fs;
use std::path::PathBuf;

#[test]
#[ignore = "requires AGENTD_CAPTURED_PROCFS_DIR from scripts/real-smoke.sh"]
fn replays_four_real_shared_cwd_processes() {
    let root = PathBuf::from(
        std::env::var_os("AGENTD_CAPTURED_PROCFS_DIR")
            .expect("AGENTD_CAPTURED_PROCFS_DIR is required"),
    );
    let expected_cwd = fs::read_to_string(root.join("cwd"))
        .unwrap()
        .trim_end()
        .to_owned();
    let pids: Vec<u32> = fs::read_to_string(root.join("pids"))
        .unwrap()
        .lines()
        .map(|line| line.parse().unwrap())
        .collect();
    assert_eq!(pids.len(), 4);
    let mut codex = 0;
    let mut claude = 0;
    for pid in pids {
        let directory = root.join(pid.to_string());
        let stat = parse_stat(&fs::read_to_string(directory.join("stat")).unwrap(), pid).unwrap();
        let uid =
            parse_effective_uid(&fs::read_to_string(directory.join("status")).unwrap()).unwrap();
        let cwd = fs::read_to_string(directory.join("cwd"))
            .unwrap()
            .trim_end()
            .to_owned();
        assert_eq!(uid, unsafe { libc::geteuid() as u32 });
        assert_eq!(cwd, expected_cwd);
        match Harness::from_comm(&stat.comm).unwrap() {
            Harness::Codex => codex += 1,
            Harness::Claude => claude += 1,
        }
    }
    assert_eq!(codex, 3);
    assert_eq!(claude, 1);
}
