use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const AUTHORITATIVE_SKILL_SHA256: &str =
    "8b7079cb7de05984958b55124f7642ab92a4b8baf5974214e9b9cc85ffa78654";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(relative: &str) -> String {
    fs::read_to_string(root().join(relative)).unwrap()
}

fn sha256(input: &[u8]) -> String {
    let mut child = Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned()
}

#[test]
fn operator_skill_frontmatter_source_delta_and_privacy_are_exact() {
    let skill = read("skills/agentd/SKILL.md");
    let mut lines = skill.lines();
    assert_eq!(lines.next(), Some("---"));
    assert_eq!(lines.next(), Some("name: agentd"));
    assert!(
        lines
            .next()
            .unwrap()
            .starts_with("description: Operate Agentd,")
    );
    assert_eq!(lines.next(), Some("---"));

    let name_request = concat!(
        "{\"version\":1,\"op\":\"name\",\"agent\":{\"pid\":12345,",
        "\"startTimeTicks\":987654},\"name\":\"Review lane\"}\n"
    );
    let name_contract = concat!(
        "- `name`: set a display name with a string or clear it with `null`; one\n",
        "  acknowledgement or one error frame, then close.\n"
    );
    let corrected_errors = concat!(
        "  (`unsupported_version`, `unknown_operation`, `malformed_request`,\n",
        "  `request_too_large`, `invalid_activity`, `invalid_name`, `unknown_agent`,\n",
        "  `name_store_unavailable`); the message never\n"
    );
    let source_errors = concat!(
        "  (`unsupported_version`, `unknown_operation`, `malformed_request`,\n",
        "  `request_too_large`, `invalid_activity`, `unknown_agent`); the message never\n"
    );
    assert_eq!(skill.matches(name_request).count(), 1);
    assert_eq!(skill.matches(name_contract).count(), 1);
    assert_eq!(skill.matches(corrected_errors).count(), 1);

    let reconstructed = skill
        .replacen(name_request, "", 1)
        .replacen(name_contract, "", 1)
        .replacen(corrected_errors, source_errors, 1);
    assert_eq!(sha256(reconstructed.as_bytes()), AUTHORITATIVE_SKILL_SHA256);

    let lower = skill.to_ascii_lowercase();
    for forbidden in [
        "mike",
        "flynn",
        "clickety-clacks",
        "tightbeam",
        "clawline",
        "gibson",
        "osanwe",
        "eezo",
        "/home/",
        "/users/",
        "/root/",
        ".tightbeam",
        "asg_",
        "wi_",
        "att_",
        "art_",
        "dr_",
        "ghp_",
        "sk-",
    ] {
        assert!(
            !lower.contains(forbidden),
            "forbidden site data: {forbidden}"
        );
    }
}

#[test]
fn version_and_install_authorization_are_consistent() {
    let cargo = read("Cargo.toml");
    let lock = read("Cargo.lock");
    let readme = read("README.md");
    assert!(cargo.contains(&format!("version = \"{VERSION}\"")));
    assert!(lock.contains(&format!("name = \"agentd\"\nversion = \"{VERSION}\"")));
    assert!(readme.contains(&format!(
        "The current Agentd product release is v{VERSION}."
    )));
    assert!(readme.contains("skills/agentd/SKILL.md"));
    assert!(readme.contains("explicitly authorizes that\naction"));
    assert!(readme.contains("Never install it silently."));
    assert!(readme.contains("Each agent environment has its own documented skill discovery"));
}

fn package(output: &Path, dry_run: bool) -> std::process::Output {
    let mut command = Command::new(root().join("scripts/package-release.sh"));
    command
        .arg("--binary")
        .arg(env!("CARGO_BIN_EXE_agentd"))
        .arg("--output-dir")
        .arg(output)
        .arg("--source-date-epoch")
        .arg("0");
    if dry_run {
        command.arg("--dry-run");
    }
    command.output().unwrap()
}

#[test]
fn release_archive_manifest_modes_receipt_and_bytes_are_reproducible() {
    let scratch = root()
        .join("target")
        .join(format!("release-package-test-{}", std::process::id()));
    let first = scratch.join("first");
    let second = scratch.join("second");
    if scratch.exists() {
        fs::remove_dir_all(&scratch).unwrap();
    }

    let dry_run = package(&first, true);
    assert!(
        dry_run.status.success(),
        "{}",
        String::from_utf8_lossy(&dry_run.stderr)
    );
    assert!(!first.exists());
    let dry_stdout = String::from_utf8(dry_run.stdout).unwrap();
    assert!(dry_stdout.contains(&format!("version={VERSION}")));
    assert!(dry_stdout.contains(&format!("mode=0644 path=agentd-{VERSION}-")));
    assert!(dry_stdout.contains("/skills/agentd/SKILL.md"));

    for output in [&first, &second] {
        let result = package(output, false);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    let archive = fs::read_dir(&first)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|extension| extension == "gz"))
        .unwrap();
    let archive_name = archive.file_name().unwrap();
    let second_archive = second.join(archive_name);
    assert_eq!(
        fs::read(&archive).unwrap(),
        fs::read(&second_archive).unwrap()
    );

    let receipt = fs::read_to_string(first.join("SHA256SUMS")).unwrap();
    assert_eq!(
        receipt,
        format!(
            "{}  {}\n",
            sha256(&fs::read(&archive).unwrap()),
            archive_name.to_string_lossy()
        )
    );

    let listing = Command::new("tar")
        .arg("-tzf")
        .arg(&archive)
        .output()
        .unwrap();
    assert!(listing.status.success());
    let listing = String::from_utf8(listing.stdout).unwrap();
    let package_name = archive_name
        .to_string_lossy()
        .strip_suffix(".tar.gz")
        .unwrap()
        .to_owned();
    let expected = [
        format!("{package_name}/"),
        format!("{package_name}/README.md"),
        format!("{package_name}/agentd"),
        format!("{package_name}/packaging/"),
        format!("{package_name}/packaging/systemd/"),
        format!("{package_name}/packaging/systemd/agentd.service"),
        format!("{package_name}/skills/"),
        format!("{package_name}/skills/agentd/"),
        format!("{package_name}/skills/agentd/SKILL.md"),
    ];
    assert_eq!(listing.lines().collect::<Vec<_>>(), expected);

    let verbose = Command::new("tar")
        .arg("-tvzf")
        .arg(&archive)
        .output()
        .unwrap();
    assert!(verbose.status.success());
    let verbose = String::from_utf8(verbose.stdout).unwrap();
    for line in verbose.lines() {
        let mode = if line.ends_with(&format!("{package_name}/agentd")) {
            "-rwxr-xr-x"
        } else if line.ends_with('/') {
            "drwxr-xr-x"
        } else {
            "-rw-r--r--"
        };
        assert!(line.starts_with(mode), "unexpected archive mode: {line}");
    }

    let skill = Command::new("tar")
        .arg("-xOzf")
        .arg(&archive)
        .arg(format!("{package_name}/skills/agentd/SKILL.md"))
        .output()
        .unwrap();
    assert!(skill.status.success());
    assert_eq!(
        skill.stdout,
        fs::read(root().join("skills/agentd/SKILL.md")).unwrap()
    );

    fs::remove_dir_all(&scratch).unwrap();
}
