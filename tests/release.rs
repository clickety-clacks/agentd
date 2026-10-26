use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn root() -> PathBuf {
    std::env::current_dir().expect("release tests run from the candidate repository root")
}

fn agentd_binary() -> PathBuf {
    root().join("target/debug/agentd")
}

fn agentd_attention_binary() -> PathBuf {
    root().join("target/debug/agentd-attention")
}

fn package(output: &Path, attention_binary: &Path) -> std::process::Output {
    let mut command = Command::new(root().join("scripts/package-release.sh"));
    command
        .arg("--binary")
        .arg(agentd_binary())
        .arg("--attention-binary")
        .arg(attention_binary)
        .arg("--output-dir")
        .arg(output)
        .args(["--source-date-epoch", "0"]);
    command.output().unwrap()
}

#[test]
fn optional_attention_archive_contains_binary_docs_and_service_unit() {
    let scratch = root()
        .join("target")
        .join(format!(
            "release-package-test-{}-with-attention",
            std::process::id()
        ));
    let first = scratch.join("first");
    if scratch.exists() {
        fs::remove_dir_all(&scratch).unwrap();
    }

    let result = package(&first, &agentd_attention_binary());
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    let archive = fs::read_dir(&first)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|extension| extension == "gz"))
        .unwrap();
    let archive_name = archive.file_name().unwrap();
    assert!(archive_name
        .to_string_lossy()
        .starts_with(&format!("agentd-{VERSION}-")));
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
    for entry in [
        format!("{package_name}/agentd-attention"),
        format!("{package_name}/docs/agentd-attention.md"),
        format!("{package_name}/packaging/systemd/agentd-attention.service"),
    ] {
        assert!(
            listing.lines().any(|line| line == entry.as_str()),
            "missing {entry}"
        );
    }

    let verbose = Command::new("tar")
        .arg("-tvzf")
        .arg(&archive)
        .output()
        .unwrap();
    assert!(verbose.status.success());
    let verbose = String::from_utf8(verbose.stdout).unwrap();
    let bridge_mode = verbose
        .lines()
        .find(|line| line.ends_with(&format!("{package_name}/agentd-attention")))
        .expect("bridge binary is listed in the package");
    assert!(bridge_mode.starts_with("-rwxr-xr-x"));

    let extracted = scratch.join("extracted");
    fs::create_dir(&extracted).unwrap();
    let extraction = Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(&extracted)
        .output()
        .unwrap();
    assert!(
        extraction.status.success(),
        "{}",
        String::from_utf8_lossy(&extraction.stderr)
    );
    let packaged_bridge = Command::new(extracted.join(&package_name).join("agentd-attention"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(
        packaged_bridge.status.success(),
        "{}",
        String::from_utf8_lossy(&packaged_bridge.stderr)
    );
    assert_eq!(
        String::from_utf8(packaged_bridge.stdout).unwrap(),
        format!("agentd-attention {VERSION}\n")
    );

    let bridge_doc = Command::new("tar")
        .arg("-xOzf")
        .arg(&archive)
        .arg(format!("{package_name}/docs/agentd-attention.md"))
        .output()
        .unwrap();
    assert!(bridge_doc.status.success());
    assert_eq!(
        bridge_doc.stdout,
        fs::read(root().join("docs/agentd-attention.md")).unwrap()
    );

    let service_unit = Command::new("tar")
        .arg("-xOzf")
        .arg(&archive)
        .arg(format!("{package_name}/packaging/systemd/agentd-attention.service"))
        .output()
        .unwrap();
    assert!(service_unit.status.success());
    assert_eq!(
        service_unit.stdout,
        fs::read(root().join("packaging/systemd/agentd-attention.service")).unwrap()
    );

    fs::remove_dir_all(&scratch).unwrap();
}

#[test]
fn core_archive_succeeds_when_optional_attention_binary_is_absent() {
    let scratch = root().join("target").join(format!(
        "release-package-test-{}-without-attention",
        std::process::id()
    ));
    let output_dir = scratch.join("archive");
    let missing_attention = scratch.join("missing-agentd-attention");
    if scratch.exists() {
        fs::remove_dir_all(&scratch).unwrap();
    }

    let result = package(&output_dir, &missing_attention);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stderr)
        .contains("warning: optional attention payload omitted"));

    let archive = fs::read_dir(&output_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|extension| extension == "gz"))
        .unwrap();
    let package_name = archive
        .file_name()
        .unwrap()
        .to_string_lossy()
        .strip_suffix(".tar.gz")
        .unwrap()
        .to_owned();
    let listing = Command::new("tar")
        .arg("-tzf")
        .arg(&archive)
        .output()
        .unwrap();
    assert!(listing.status.success());
    let listing = String::from_utf8(listing.stdout).unwrap();
    for entry in [
        format!("{package_name}/agentd"),
        format!("{package_name}/README.md"),
        format!("{package_name}/packaging/systemd/agentd.service"),
        format!("{package_name}/skills/agentd/SKILL.md"),
    ] {
        assert!(
            listing.lines().any(|line| line == entry.as_str()),
            "missing core entry {entry}"
        );
    }
    for entry in [
        format!("{package_name}/agentd-attention"),
        format!("{package_name}/docs/agentd-attention.md"),
        format!("{package_name}/packaging/systemd/agentd-attention.service"),
    ] {
        assert!(
            !listing.lines().any(|line| line == entry.as_str()),
            "unexpected optional entry {entry}"
        );
    }

    let extracted = scratch.join("extracted");
    fs::create_dir(&extracted).unwrap();
    let extraction = Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(&extracted)
        .output()
        .unwrap();
    assert!(
        extraction.status.success(),
        "{}",
        String::from_utf8_lossy(&extraction.stderr)
    );
    let packaged_agentd = Command::new(extracted.join(&package_name).join("agentd"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(
        packaged_agentd.status.success(),
        "{}",
        String::from_utf8_lossy(&packaged_agentd.stderr)
    );
    assert_eq!(
        String::from_utf8(packaged_agentd.stdout).unwrap(),
        format!("agentd {VERSION}\n")
    );

    fs::remove_dir_all(&scratch).unwrap();
}
