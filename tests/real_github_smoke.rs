use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn run(home: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nv"))
        .args(arguments)
        .env("HOME", home)
        .output()
        .unwrap()
}

fn succeed(home: &Path, arguments: &[&str]) -> Output {
    let output = run(home, arguments);
    assert!(
        output.status.success(),
        "command {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
#[ignore = "downloads real stable and nightly Neovim releases"]
fn real_github_smoke() {
    let home = PathBuf::from(
        std::env::var_os("NV_REAL_SMOKE_HOME")
            .expect("set NV_REAL_SMOKE_HOME to a dedicated absolute test home"),
    );
    assert!(home.is_absolute(), "NV_REAL_SMOKE_HOME must be absolute");
    assert_ne!(
        Some(home.as_os_str()),
        std::env::var_os("HOME").as_deref(),
        "NV_REAL_SMOKE_HOME must not be the real HOME"
    );
    std::fs::create_dir_all(&home).unwrap();

    succeed(&home, &["install", "stable"]);
    succeed(&home, &["install", "nightly"]);
    succeed(&home, &["use", "stable"]);
    succeed(&home, &["use", "nightly"]);

    let stable_no_op = succeed(&home, &["update", "stable"]);
    assert!(String::from_utf8_lossy(&stable_no_op.stdout).contains("already current"));
    let nightly_no_op = succeed(&home, &["update", "nightly"]);
    assert!(String::from_utf8_lossy(&nightly_no_op.stdout).contains("already current"));

    let status = succeed(&home, &["status"]);
    let status = String::from_utf8(status.stdout).unwrap();
    assert!(status.contains("active: nightly"));
    assert!(status.contains("stable current: release="));
    assert!(status.contains("nightly current: release="));

    if !status.contains("nightly previous: none") {
        succeed(&home, &["rollback", "nightly"]);
        succeed(&home, &["rollback", "nightly"]);
    } else {
        eprintln!(
            "nightly rollback will be exercised on a later run after the retained smoke home receives a newer nightly release"
        );
    }
}
