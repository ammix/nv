use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io::{ErrorKind, IsTerminal};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const ASSET_NAME: &str = "nvim-linux-x86_64.tar.gz";
const NVIM_LINK_TARGET: &str = "../share/nv/active/bin/nvim";
const USAGE: &str = "Usage:
  nv install stable|nightly
  nv use stable|nightly
  nv update [stable|nightly]
  nv rollback stable|nightly
  nv status
  nv help";

type Result<T> = std::result::Result<T, Box<dyn Error>>;

trait WithPath<T> {
    fn with_path(self, operation: &str, path: &Path) -> Result<T>;
}

impl<T> WithPath<T> for std::io::Result<T> {
    fn with_path(self, operation: &str, path: &Path) -> Result<T> {
        self.map_err(|cause| format!("{operation} {}: {cause}", path.display()).into())
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Channel {
    Stable,
    Nightly,
}

impl Channel {
    const ALL: [Self; 2] = [Self::Stable, Self::Nightly];

    fn parse(value: &str) -> Result<Self> {
        match value {
            "stable" => Ok(Self::Stable),
            "nightly" => Ok(Self::Nightly),
            _ => Err(format!("unsupported channel '{value}'; expected stable or nightly").into()),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Nightly => "nightly",
        }
    }

    fn api_url(self) -> &'static str {
        match self {
            Self::Stable => "https://api.github.com/repos/neovim/neovim/releases/latest",
            Self::Nightly => "https://api.github.com/repos/neovim/neovim/releases/tags/nightly",
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

struct Paths {
    installs: PathBuf,
    channels: PathBuf,
    staging: PathBuf,
    active: PathBuf,
    nvim_link: PathBuf,
}

impl Paths {
    fn create() -> Result<Self> {
        let home = PathBuf::from(env::var_os("HOME").ok_or("HOME is not set")?);
        let state = home.join(".local/share/nv");
        let paths = Self {
            installs: state.join("installs"),
            channels: state.join("channels"),
            staging: state.join("staging"),
            active: state.join("active"),
            nvim_link: home.join(".local/bin/nvim"),
        };
        for dir in [
            paths.installs.clone(),
            paths.channels.join("stable"),
            paths.channels.join("nightly"),
            home.join(".local/bin"),
        ] {
            fs::create_dir_all(&dir).with_path("failed to create", &dir)?;
        }
        Ok(paths)
    }

    fn channel_link(&self, channel: Channel, name: &str) -> PathBuf {
        self.channels.join(channel.as_str()).join(name)
    }
}

struct Release {
    id: String,
    url: String,
    sha256: String,
}

fn main() {
    if let Err(cause) = run() {
        eprintln!("nv: {cause}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    if let ["help" | "--help" | "-h"] = args.as_slice() {
        println!("{USAGE}");
        return Ok(());
    }
    let paths = Paths::create()?;
    match args.as_slice() {
        ["install", channel] => install(&paths, Channel::parse(channel)?),
        ["use", channel] => {
            let channel = Channel::parse(channel)?;
            install(&paths, channel)?;
            activate(&paths, channel)
        }
        ["update"] => update(&paths, None),
        ["update", channel] => update(&paths, Some(Channel::parse(channel)?)),
        ["rollback", channel] => rollback(&paths, Channel::parse(channel)?),
        ["status"] => status(&paths),
        _ => Err(format!("invalid arguments\n\n{USAGE}").into()),
    }
}

fn install(paths: &Paths, channel: Channel) -> Result<()> {
    let current = read_pointer(paths, channel, "current")?;
    let _ = fs::remove_dir_all(&paths.staging);
    fs::create_dir(&paths.staging).with_path("failed to create", &paths.staging)?;
    let release = resolve_release(channel, &paths.staging)?;
    let name = format!("{channel}-{}", release.id);
    let target = paths.installs.join(&name);
    if current.as_deref() == Some(name.as_str()) {
        println!(
            "{channel} is already current: {} (release {})",
            nvim_version(&target)?,
            release.id
        );
    } else {
        let version = if target.exists() {
            nvim_version(&target)?
        } else {
            fetch(&release, &paths.staging, &target)?
        };
        if let Some(current) = current {
            write_pointer(paths, channel, "previous", &current)?;
        }
        write_pointer(paths, channel, "current", &name)?;
        cleanup(paths)?;
        println!("installed {channel} {version} (release {})", release.id);
    }
    fs::remove_dir_all(&paths.staging).with_path("failed to remove", &paths.staging)
}

fn resolve_release(channel: Channel, staging: &Path) -> Result<Release> {
    let response = staging.join("release.json");
    curl(channel.api_url(), &response, false)?;
    let filter = r#".id, (.assets[] | select(.name == $asset) | .browser_download_url, (.digest | ltrimstr("sha256:")))"#;
    let output = execute(
        Command::new("jq")
            .args([
                "--raw-output",
                "--exit-status",
                "--arg",
                "asset",
                ASSET_NAME,
                filter,
            ])
            .arg(&response),
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let &[id, url, sha256] = stdout.lines().collect::<Vec<_>>().as_slice() else {
        return Err(format!("unexpected release metadata in {}", response.display()).into());
    };
    Ok(Release {
        id: id.to_owned(),
        url: url.to_owned(),
        sha256: sha256.to_owned(),
    })
}

fn curl(url: &str, destination: &Path, progress: bool) -> Result<()> {
    let mut command = Command::new("curl");
    command
        .args([
            "--fail",
            "--show-error",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
        ])
        .args(["--connect-timeout", "15", "--max-time", "600", "--output"])
        .arg(destination);
    if progress && std::io::stderr().is_terminal() {
        command.arg("--progress-bar").stderr(Stdio::inherit());
    } else {
        command.arg("--silent");
    }
    execute(command.arg(url))?;
    Ok(())
}

fn execute(command: &mut Command) -> Result<Output> {
    let program = command.get_program().to_string_lossy().into_owned();
    let output = command
        .output()
        .map_err(|cause| format!("failed to run {program}: {cause}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("{program} failed ({}): {}", output.status, stderr.trim()).into());
    }
    Ok(output)
}

fn fetch(release: &Release, staging: &Path, target: &Path) -> Result<String> {
    let archive = staging.join(ASSET_NAME);
    curl(&release.url, &archive, true)?;
    let output = execute(Command::new("sha256sum").arg(&archive))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let actual = stdout.split_whitespace().next().unwrap_or_default();
    if actual != release.sha256 {
        return Err(format!(
            "SHA-256 mismatch for {}: expected {}, got {actual}",
            archive.display(),
            release.sha256
        )
        .into());
    }
    let extracted = staging.join("extracted");
    fs::create_dir(&extracted).with_path("failed to create", &extracted)?;
    execute(
        Command::new("tar")
            .args(["--extract", "--gzip", "--strip-components=1", "--file"])
            .arg(&archive)
            .arg("--directory")
            .arg(&extracted),
    )?;
    let version = nvim_version(&extracted)?;
    fs::rename(&extracted, target).with_path("failed to move installation to", target)?;
    Ok(version)
}

fn nvim_version(install: &Path) -> Result<String> {
    let output = execute(Command::new(install.join("bin/nvim")).arg("--version"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.lines().next().unwrap_or_default().to_owned())
}

fn release_id(install: &str) -> &str {
    install.split_once('-').map_or(install, |(_, id)| id)
}

fn read_pointer(paths: &Paths, channel: Channel, name: &str) -> Result<Option<String>> {
    let link = paths.channel_link(channel, name);
    match fs::read_link(&link) {
        Ok(target) => Ok(Some(
            target
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        )),
        Err(cause) if cause.kind() == ErrorKind::NotFound => Ok(None),
        Err(cause) => Err(format!("failed to read {}: {cause}", link.display()).into()),
    }
}

fn write_pointer(paths: &Paths, channel: Channel, name: &str, install: &str) -> Result<()> {
    replace_link(
        &paths.channel_link(channel, name),
        &Path::new("../../installs").join(install),
    )
}

fn replace_link(link: &Path, target: &Path) -> Result<()> {
    let temporary = link.with_extension("new");
    let _ = fs::remove_file(&temporary);
    symlink(target, &temporary).with_path("failed to create symlink", &temporary)?;
    fs::rename(&temporary, link).with_path("failed to replace", link)
}

fn activate(paths: &Paths, channel: Channel) -> Result<()> {
    let current = read_pointer(paths, channel, "current")?
        .ok_or_else(|| format!("{channel} is not installed"))?;
    if fs::symlink_metadata(&paths.nvim_link).is_ok_and(|metadata| !metadata.is_symlink()) {
        return Err(format!(
            "{} exists and is not a symlink; refusing to replace it",
            paths.nvim_link.display()
        )
        .into());
    }
    replace_link(&paths.active, &active_target(channel))?;
    replace_link(&paths.nvim_link, Path::new(NVIM_LINK_TARGET))?;
    println!(
        "using {channel} {} (release {})",
        nvim_version(&paths.installs.join(&current))?,
        release_id(&current)
    );
    Ok(())
}

fn active_target(channel: Channel) -> PathBuf {
    PathBuf::from(format!("channels/{channel}/current"))
}

fn active_channel(paths: &Paths) -> Option<Channel> {
    let target = fs::read_link(&paths.active).ok()?;
    Channel::ALL
        .into_iter()
        .find(|channel| target == active_target(*channel))
}

fn installed(paths: &Paths, selection: Option<Channel>) -> Result<Vec<Channel>> {
    let mut channels = Vec::new();
    for channel in Channel::ALL {
        if selection.is_none_or(|selected| selected == channel)
            && read_pointer(paths, channel, "current")?.is_some()
        {
            channels.push(channel);
        }
    }
    if channels.is_empty() {
        return Err(match selection {
            Some(channel) => format!("{channel} is not installed"),
            None => "no channels are installed".to_owned(),
        }
        .into());
    }
    Ok(channels)
}

fn update(paths: &Paths, selection: Option<Channel>) -> Result<()> {
    for channel in installed(paths, selection)? {
        install(paths, channel)?;
    }
    Ok(())
}

fn rollback(paths: &Paths, channel: Channel) -> Result<()> {
    let current = read_pointer(paths, channel, "current")?
        .ok_or_else(|| format!("{channel} is not installed"))?;
    let previous = read_pointer(paths, channel, "previous")?
        .ok_or_else(|| format!("{channel} has no previous installation"))?;
    write_pointer(paths, channel, "previous", &current)?;
    write_pointer(paths, channel, "current", &previous)?;
    println!(
        "rolled back {channel} to {} (release {})",
        nvim_version(&paths.installs.join(&previous))?,
        release_id(&previous)
    );
    Ok(())
}

fn cleanup(paths: &Paths) -> Result<()> {
    let mut referenced = Vec::new();
    for channel in Channel::ALL {
        for name in ["current", "previous"] {
            referenced.extend(read_pointer(paths, channel, name)?);
        }
    }
    for entry in fs::read_dir(&paths.installs).with_path("failed to read", &paths.installs)? {
        let path = entry.with_path("failed to read", &paths.installs)?.path();
        if !referenced.iter().any(|name| path.ends_with(name)) {
            fs::remove_dir_all(&path).with_path("failed to remove", &path)?;
        }
    }
    Ok(())
}

fn status(paths: &Paths) -> Result<()> {
    println!(
        "active: {}",
        active_channel(paths).map_or("none", Channel::as_str)
    );
    for channel in Channel::ALL {
        for name in ["current", "previous"] {
            let entry = read_pointer(paths, channel, name)?.and_then(|install| {
                let version = nvim_version(&paths.installs.join(&install)).ok()?;
                Some(format!(
                    "release={} version={version}",
                    release_id(&install)
                ))
            });
            println!("{channel} {name}: {}", entry.as_deref().unwrap_or("none"));
        }
    }
    Ok(())
}
