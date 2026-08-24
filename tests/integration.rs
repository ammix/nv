use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const CURL: &str = r#"#!/bin/sh
for url; do :; done
while [ "$1" != --output ]; do shift; done
case $url in
  https://api.github.com/*)
    printf '{"id":%s,"assets":[{"name":"nvim-linux-x86_64.tar.gz","browser_download_url":"https://example.invalid/nvim.tar.gz","digest":"sha256:good"}]}' "$RELEASE" > "$2" ;;
  *) : > "$2" ;;
esac
"#;

const SHA256SUM: &str = r#"#!/bin/sh
printf '%s  %s\n' "${SHA:-good}" "$1"
"#;

const TAR: &str = r#"#!/bin/sh
for dir; do :; done
mkdir -p "$dir/bin"
printf '#!/bin/sh\necho "%s"\n' "$VERSION" > "$dir/bin/nvim"
chmod 755 "$dir/bin/nvim"
"#;

struct Sandbox {
    home: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let home = std::env::temp_dir().join(format!("nv-test-{}-{name}", std::process::id()));
        fs::create_dir_all(home.join("fake")).unwrap();
        for (program, script) in [("curl", CURL), ("sha256sum", SHA256SUM), ("tar", TAR)] {
            let path = home.join("fake").join(program);
            fs::write(&path, script).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self { home }
    }

    fn nv(&self, args: &[&str], release: &str, version: &str, sha: &str) -> Output {
        let path = std::env::var("PATH").unwrap();
        Command::new(env!("CARGO_BIN_EXE_nv"))
            .args(args)
            .env("HOME", &self.home)
            .env(
                "PATH",
                format!("{}:{path}", self.home.join("fake").display()),
            )
            .env("RELEASE", release)
            .env("VERSION", version)
            .env("SHA", sha)
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str], release: &str, version: &str) {
        let output = self.nv(args, release, version, "good");
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn state(&self, path: &str) -> PathBuf {
        self.home.join(".local/share/nv").join(path)
    }

    fn link(&self, path: &str) -> PathBuf {
        fs::read_link(self.state(path)).unwrap()
    }

    fn version(&self) -> String {
        let output = Command::new(self.home.join(".local/bin/nvim"))
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.home);
    }
}

#[test]
fn lifecycle() {
    let sandbox = Sandbox::new("lifecycle");
    sandbox.ok(&["install", "stable"], "100", "v1");
    assert!(!sandbox.home.join(".local/bin/nvim").exists());
    sandbox.ok(&["use", "stable"], "100", "v1");
    assert_eq!(sandbox.link("active"), Path::new("channels/stable/current"));
    assert_eq!(sandbox.version(), "v1\n");

    sandbox.ok(&["update"], "101", "v2");
    assert_eq!(
        sandbox.link("channels/stable/current"),
        Path::new("../../installs/stable-101")
    );
    assert_eq!(
        sandbox.link("channels/stable/previous"),
        Path::new("../../installs/stable-100")
    );
    assert_eq!(sandbox.version(), "v2\n");

    sandbox.ok(&["rollback", "stable"], "101", "v2");
    assert_eq!(sandbox.version(), "v1\n");

    sandbox.ok(&["update", "stable"], "102", "v3");
    assert_eq!(fs::read_dir(sandbox.state("installs")).unwrap().count(), 2);
    assert!(!sandbox.state("installs/stable-101").exists());
}

#[test]
fn failed_update_keeps_the_active_install() {
    let sandbox = Sandbox::new("failure");
    sandbox.ok(&["use", "stable"], "100", "v1");
    let output = sandbox.nv(&["update", "stable"], "101", "v2", "bad");
    assert!(!output.status.success());
    assert_eq!(sandbox.version(), "v1\n");
    assert!(!sandbox.state("installs/stable-101").exists());
}
