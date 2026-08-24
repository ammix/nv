use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TestEnvironment {
    root: PathBuf,
    home: PathBuf,
    fake_bin: PathBuf,
}

impl TestEnvironment {
    fn new() -> Self {
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("nv-integration-{}-{sequence}", std::process::id()));
        let home = root.join("home");
        let fake_bin = root.join("fake-bin");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&fake_bin).unwrap();
        let environment = Self {
            root,
            home,
            fake_bin,
        };
        environment.install_fake_programs();
        environment
    }

    fn install_fake_programs(&self) {
        self.write_executable(
            "curl",
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  printf '%s\n' 'fake curl'
  exit 0
fi
output=
want_output=0
last=
for argument in "$@"; do
  if [ "$want_output" = 1 ]; then
    output=$argument
    want_output=0
  elif [ "$argument" = "--output" ]; then
    want_output=1
  fi
  last=$argument
done
case "$last" in
  https://api.github.com/*)
    printf '%s' '{"release":"fake"}' > "$output"
    ;;
  *)
    printf '%s' "${FAKE_PAYLOAD:-payload}" > "$output"
    ;;
esac
"#,
        );
        self.write_executable(
            "jq",
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  printf '%s\n' 'fake jq'
  exit 0
fi
printf '%s\n' "${FAKE_RELEASE_ID:-100}"
printf '%s\n' 'https://github.com/neovim/neovim/releases/download/fake/nvim-linux-x86_64.tar.gz'
printf '%s\n' "sha256:${FAKE_DIGEST:-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa}"
printf '%s\n' "${FAKE_SIZE:-7}"
"#,
        );
        self.write_executable(
            "sha256sum",
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  printf '%s\n' 'fake sha256sum'
  exit 0
fi
if [ "${FAKE_MODE:-}" = checksum_failure ]; then
  printf '%s  %s\n' 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb' "$3"
  exit 0
fi
printf '%s  %s\n' "${FAKE_DIGEST:-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa}" "$3"
"#,
        );
        self.write_executable(
            "tar",
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  printf '%s\n' 'fake tar'
  exit 0
fi
if [ "${FAKE_MODE:-}" = extraction_failure ]; then
  printf '%s\n' 'tar: extraction failed' >&2
  exit 2
fi
destination=
want_destination=0
for argument in "$@"; do
  if [ "$want_destination" = 1 ]; then
    destination=$argument
    want_destination=0
  elif [ "$argument" = "--directory" ]; then
    want_destination=1
  fi
done
/usr/bin/mkdir -p "$destination/nvim-linux-x86_64/bin"
binary="$destination/nvim-linux-x86_64/bin/nvim"
printf '%s\n' '#!/bin/sh' > "$binary"
if [ "${FAKE_MODE:-}" = version_failure ]; then
  printf '%s\n' "printf '%s\\n' 'broken staged nvim' >&2" >> "$binary"
  printf '%s\n' 'exit 9' >> "$binary"
else
  printf '%s\n' "printf '%s\\n' '${FAKE_VERSION:-NVIM v0.1.0}'" >> "$binary"
fi
/usr/bin/chmod 755 "$binary"
"#,
        );
    }

    fn write_executable(&self, name: &str, contents: &str) {
        let path = self.fake_bin.join(name);
        fs::write(&path, contents).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    fn run(&self, arguments: &[&str], release: &str, version: &str, mode: &str) -> Output {
        let path = format!("{}:/usr/bin:/bin", self.fake_bin.display());
        Command::new(env!("CARGO_BIN_EXE_nv"))
            .args(arguments)
            .env("HOME", &self.home)
            .env("PATH", path)
            .env("FAKE_RELEASE_ID", release)
            .env("FAKE_VERSION", version)
            .env("FAKE_DIGEST", DIGEST)
            .env("FAKE_MODE", mode)
            .output()
            .unwrap()
    }

    fn success(&self, arguments: &[&str], release: &str, version: &str) -> Output {
        let output = self.run(arguments, release, version, "");
        assert!(
            output.status.success(),
            "command {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn state(&self) -> PathBuf {
        self.home.join(".local/share/nv")
    }

    fn channel_target(&self, channel: &str, pointer: &str) -> PathBuf {
        fs::read_link(self.state().join("channels").join(channel).join(pointer)).unwrap()
    }

    fn exposed_version(&self) -> String {
        let output = Command::new(self.home.join(".local/bin/nvim"))
            .arg("--version")
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    }
}

impl Drop for TestEnvironment {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn install_does_not_activate_and_use_installs_and_activates() {
    let environment = TestEnvironment::new();
    environment.success(&["install", "stable"], "100", "NVIM v1.0.0");
    assert!(!environment.state().join("active").exists());
    assert!(!environment.home.join(".local/bin/nvim").exists());

    environment.success(&["use", "nightly"], "200", "NVIM v2.0.0-dev");
    assert_eq!(
        fs::read_link(environment.state().join("active")).unwrap(),
        Path::new("channels/nightly/current")
    );
    assert_eq!(environment.exposed_version(), "NVIM v2.0.0-dev\n");
    let status = environment.success(&["status"], "unused", "unused");
    let stdout = String::from_utf8(status.stdout).unwrap();
    assert!(stdout.contains("active: nightly"));
    assert!(stdout.contains("stable current: release=100 version=NVIM v1.0.0"));
    assert!(stdout.contains("nightly current: release=200 version=NVIM v2.0.0-dev"));
}

#[test]
fn update_rotates_once_rollback_swaps_and_cleanup_is_bounded() {
    let environment = TestEnvironment::new();
    environment.success(&["use", "stable"], "100", "NVIM v1.0.0");
    environment.success(&["update", "stable"], "101", "NVIM v1.1.0");
    assert_eq!(
        environment.channel_target("stable", "current"),
        Path::new("../../installs/stable-101")
    );
    assert_eq!(
        environment.channel_target("stable", "previous"),
        Path::new("../../installs/stable-100")
    );
    assert_eq!(environment.exposed_version(), "NVIM v1.1.0\n");

    let no_op = environment.success(&["update", "stable"], "101", "NVIM v1.1.0");
    assert!(String::from_utf8_lossy(&no_op.stdout).contains("already current"));
    assert_eq!(
        environment.channel_target("stable", "previous"),
        Path::new("../../installs/stable-100")
    );

    environment.success(&["rollback", "stable"], "unused", "NVIM v0.0.0");
    assert_eq!(environment.exposed_version(), "NVIM v1.0.0\n");
    environment.success(&["rollback", "stable"], "unused", "NVIM v0.0.0");
    assert_eq!(environment.exposed_version(), "NVIM v1.1.0\n");

    environment.success(&["update", "stable"], "102", "NVIM v1.2.0");
    let installs = fs::read_dir(environment.state().join("installs"))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(installs.len(), 2);
    assert!(environment.state().join("installs/stable-101").exists());
    assert!(environment.state().join("installs/stable-102").exists());
    assert!(!environment.state().join("installs/stable-100").exists());
}

#[test]
fn update_without_selection_updates_every_installed_channel() {
    let environment = TestEnvironment::new();
    environment.success(&["install", "stable"], "100", "NVIM v1.0.0");
    environment.success(&["install", "nightly"], "200", "NVIM v2.0.0-dev");

    environment.success(&["update"], "300", "NVIM v3.0.0-dev");
    assert_eq!(
        environment.channel_target("stable", "current"),
        Path::new("../../installs/stable-300")
    );
    assert_eq!(
        environment.channel_target("nightly", "current"),
        Path::new("../../installs/nightly-300")
    );
    assert_eq!(
        environment.channel_target("stable", "previous"),
        Path::new("../../installs/stable-100")
    );
    assert_eq!(
        environment.channel_target("nightly", "previous"),
        Path::new("../../installs/nightly-200")
    );
}

#[test]
fn remove_deletes_retained_releases_and_default_update_skips_removed_channels() {
    let environment = TestEnvironment::new();
    environment.success(&["install", "stable"], "100", "NVIM v1.0.0");
    environment.success(&["update", "stable"], "101", "NVIM v1.1.0");
    environment.success(&["use", "nightly"], "200", "NVIM v2.0.0-dev");

    environment.success(&["remove", "stable"], "unused", "unused");
    assert!(!environment.state().join("channels/stable/current").exists());
    assert!(
        !environment
            .state()
            .join("channels/stable/previous")
            .exists()
    );
    assert!(!environment.state().join("installs/stable-100").exists());
    assert!(!environment.state().join("installs/stable-101").exists());
    assert_eq!(environment.exposed_version(), "NVIM v2.0.0-dev\n");

    environment.success(&["update"], "201", "NVIM v2.1.0-dev");
    assert!(!environment.state().join("installs/stable-201").exists());
    assert_eq!(
        environment.channel_target("nightly", "current"),
        Path::new("../../installs/nightly-201")
    );

    environment.success(&["remove"], "unused", "unused");
    assert!(!environment.state().join("active").exists());
    assert!(!environment.home.join(".local/bin/nvim").exists());
    assert!(
        fs::read_dir(environment.state().join("installs"))
            .unwrap()
            .next()
            .is_none()
    );
    let status = environment.success(&["status"], "unused", "unused");
    let stdout = String::from_utf8(status.stdout).unwrap();
    assert!(stdout.contains("active: none"));
    assert!(stdout.contains("stable current: none"));
    assert!(stdout.contains("nightly current: none"));
}

#[test]
fn failed_updates_preserve_the_active_installation() {
    let environment = TestEnvironment::new();
    environment.success(&["use", "stable"], "100", "NVIM v1.0.0");
    let failures = [
        ("size_failure", "101", "NVIM v1.1.0"),
        ("checksum_failure", "102", "NVIM v1.2.0"),
        ("extraction_failure", "103", "NVIM v1.3.0"),
        ("version_failure", "104", "NVIM v1.4.0"),
    ];
    for (mode, release, version) in failures {
        let output = if mode == "size_failure" {
            let path = format!("{}:/usr/bin:/bin", environment.fake_bin.display());
            Command::new(env!("CARGO_BIN_EXE_nv"))
                .args(["update", "stable"])
                .env("HOME", &environment.home)
                .env("PATH", path)
                .env("FAKE_RELEASE_ID", release)
                .env("FAKE_VERSION", version)
                .env("FAKE_DIGEST", DIGEST)
                .env("FAKE_SIZE", "8")
                .output()
                .unwrap()
        } else {
            environment.run(&["update", "stable"], release, version, mode)
        };
        assert!(!output.status.success(), "failure mode {mode} succeeded");
        assert_eq!(
            environment.channel_target("stable", "current"),
            Path::new("../../installs/stable-100")
        );
        assert_eq!(environment.exposed_version(), "NVIM v1.0.0\n");
        assert!(
            fs::read_dir(environment.state().join("staging"))
                .unwrap()
                .next()
                .is_none()
        );
    }
}

#[test]
fn unsafe_state_lock_and_out_of_tree_pointer_are_rejected() {
    let environment = TestEnvironment::new();
    environment.success(&["install", "stable"], "100", "NVIM v1.0.0");
    let mut permissions = fs::metadata(environment.state()).unwrap().permissions();
    permissions.set_mode(0o770);
    fs::set_permissions(environment.state(), permissions).unwrap();
    let unsafe_state = environment.run(&["status"], "unused", "unused", "");
    assert!(!unsafe_state.status.success());
    assert!(String::from_utf8_lossy(&unsafe_state.stderr).contains("writable-by-group-or-others"));
    let mut permissions = fs::metadata(environment.state()).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(environment.state(), permissions).unwrap();

    fs::write(environment.state().join("operation.lock"), "pid=1\n").unwrap();
    let locked = environment.run(&["rollback", "stable"], "unused", "unused", "");
    assert!(!locked.status.success());
    assert!(String::from_utf8_lossy(&locked.stderr).contains("operation.lock"));
    fs::remove_file(environment.state().join("operation.lock")).unwrap();

    let current = environment.state().join("channels/stable/current");
    fs::remove_file(&current).unwrap();
    symlink("/tmp/outside", current).unwrap();
    let status = environment.run(&["status"], "unused", "unused", "");
    assert!(!status.status.success());
    assert!(String::from_utf8_lossy(&status.stderr).contains("unmanaged target"));
}

#[test]
fn interrupted_pointer_transaction_is_recovered() {
    let environment = TestEnvironment::new();
    environment.success(&["use", "stable"], "100", "NVIM v1.0.0");
    environment.success(&["update", "stable"], "101", "NVIM v1.1.0");

    let previous = environment.state().join("channels/stable/previous");
    fs::remove_file(&previous).unwrap();
    symlink("../../installs/stable-101", previous).unwrap();
    fs::write(
        environment.state().join("pointer.transaction"),
        "stable\nstable-100\nstable-101\n",
    )
    .unwrap();

    environment.success(&["status"], "unused", "unused");
    assert_eq!(
        environment.channel_target("stable", "current"),
        Path::new("../../installs/stable-100")
    );
    assert_eq!(
        environment.channel_target("stable", "previous"),
        Path::new("../../installs/stable-101")
    );
    assert!(!environment.state().join("pointer.transaction").exists());
    assert_eq!(environment.exposed_version(), "NVIM v1.0.0\n");
}
