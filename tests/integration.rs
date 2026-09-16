use std::path::Path;
use std::process::Command;

fn nv(home: &Path, args: &[&str], release: &str) -> bool {
    let path = std::env::var("PATH").unwrap();
    Command::new(env!("CARGO_BIN_EXE_nv"))
        .args(args)
        .env("HOME", home)
        .env(
            "PATH",
            format!("{}/tests/fake:{path}", env!("CARGO_MANIFEST_DIR")),
        )
        .env("RELEASE", release)
        .status()
        .unwrap()
        .success()
}

#[test]
fn lifecycle() {
    let home = std::env::temp_dir().join(format!("nv-test-{}", std::process::id()));
    let nvim = home.join(".local/bin/nvim");
    let version = || String::from_utf8(Command::new(&nvim).output().unwrap().stdout).unwrap();

    assert!(nv(&home, &["use", "stable"], "100"));
    assert_eq!(version(), "100\n");
    assert!(nv(&home, &["update"], "101"));
    assert_eq!(version(), "101\n");
    assert!(nv(&home, &["rollback", "stable"], "101"));
    assert_eq!(version(), "100\n");
    assert!(nv(&home, &["remove"], "101"));
    assert!(!nvim.exists());
    std::fs::remove_dir_all(&home).unwrap();
}
