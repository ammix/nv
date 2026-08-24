use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{IsTerminal, Read, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const ASSET_NAME: &str = "nvim-linux-x86_64.tar.gz";
const ARCHIVE_ROOT: &str = "nvim-linux-x86_64";
const GITHUB_API_VERSION: &str = "2026-03-10";
const STABLE_API_URL: &str = "https://api.github.com/repos/neovim/neovim/releases/latest";
const NIGHTLY_API_URL: &str = "https://api.github.com/repos/neovim/neovim/releases/tags/nightly";
const NVIM_LINK_TARGET: &str = "../share/nv/active/bin/nvim";
const MAX_API_RESPONSE_SIZE: u64 = 10 * 1024 * 1024;
const MAX_ASSET_SIZE: u64 = 512 * 1024 * 1024;
const API_TIMEOUT_SECONDS: u64 = 60;
const ASSET_TIMEOUT_SECONDS: u64 = 600;
const USAGE: &str = "Usage:
  nv install stable|nightly
  nv use stable|nightly
  nv update [stable|nightly]
  nv remove [stable|nightly]
  nv rollback stable|nightly
  nv status
  nv help";

type Result<T> = std::result::Result<T, NvError>;

#[derive(Debug)]
struct NvError(String);

impl fmt::Display for NvError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for NvError {}

fn error(message: impl Into<String>) -> NvError {
    NvError(message.into())
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Channel {
    Stable,
    Nightly,
}

impl Channel {
    const ALL: [Self; 2] = [Self::Stable, Self::Nightly];

    fn parse(value: &OsStr) -> Result<Self> {
        match value.to_str() {
            Some("stable") => Ok(Self::Stable),
            Some("nightly") => Ok(Self::Nightly),
            Some(value) => Err(error(format!(
                "unsupported channel '{value}'; expected stable or nightly"
            ))),
            None => Err(error("channel is not valid UTF-8")),
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
            Self::Stable => STABLE_API_URL,
            Self::Nightly => NIGHTLY_API_URL,
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Eq, PartialEq)]
enum Cli {
    Install(Channel),
    Use(Channel),
    Update(ChannelSelection),
    Remove(ChannelSelection),
    Rollback(Channel),
    Status,
    Help,
}

#[derive(Debug, Eq, PartialEq)]
enum ChannelSelection {
    Channel(Channel),
    All,
}

impl Cli {
    fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Self> {
        let arguments: Vec<OsString> = arguments.into_iter().collect();
        let command = arguments
            .first()
            .ok_or_else(|| error(format!("missing command\n\n{USAGE}")))?;
        let command = command
            .to_str()
            .ok_or_else(|| error("command is not valid UTF-8"))?;

        match command {
            "install" => Ok(Self::Install(parse_single_channel(&arguments, command)?)),
            "use" => Ok(Self::Use(parse_single_channel(&arguments, command)?)),
            "rollback" => Ok(Self::Rollback(parse_single_channel(&arguments, command)?)),
            "update" => Ok(Self::Update(parse_channel_selection(&arguments, command)?)),
            "remove" => Ok(Self::Remove(parse_channel_selection(&arguments, command)?)),
            "status" => {
                require_argument_count(&arguments, 1, command)?;
                Ok(Self::Status)
            }
            "help" | "--help" | "-h" => {
                require_argument_count(&arguments, 1, command)?;
                Ok(Self::Help)
            }
            value => Err(error(format!("unsupported command '{value}'\n\n{USAGE}"))),
        }
    }
}

fn parse_single_channel(arguments: &[OsString], command: &str) -> Result<Channel> {
    require_argument_count(arguments, 2, command)?;
    Channel::parse(&arguments[1])
}

fn parse_channel_selection(arguments: &[OsString], command: &str) -> Result<ChannelSelection> {
    if arguments.len() > 2 {
        return Err(error(format!(
            "invalid arguments for '{command}': expected at most 1, received {}\n\n{USAGE}",
            arguments.len() - 1
        )));
    }
    match arguments.get(1) {
        None => Ok(ChannelSelection::All),
        Some(value) => Ok(ChannelSelection::Channel(Channel::parse(value)?)),
    }
}

fn require_argument_count(arguments: &[OsString], expected: usize, command: &str) -> Result<()> {
    if arguments.len() == expected {
        return Ok(());
    }
    Err(error(format!(
        "invalid arguments for '{command}': expected {}, received {}\n\n{USAGE}",
        expected - 1,
        arguments.len().saturating_sub(1)
    )))
}

#[derive(Clone, Debug)]
struct Paths {
    state: PathBuf,
    installs: PathBuf,
    channels: PathBuf,
    staging: PathBuf,
    active: PathBuf,
    lock: PathBuf,
    transaction: PathBuf,
    local_bin: PathBuf,
    nvim_link: PathBuf,
}

impl Paths {
    fn discover() -> Result<Self> {
        let home = env::var_os("HOME").ok_or_else(|| error("HOME is not set"))?;
        if home.is_empty() {
            return Err(error("HOME is empty"));
        }
        Self::from_home(PathBuf::from(home))
    }

    fn from_home(home: PathBuf) -> Result<Self> {
        if !home.is_absolute() {
            return Err(error(format!(
                "HOME must be an absolute path, got {}",
                home.display()
            )));
        }
        let state = home.join(".local/share/nv");
        let local_bin = home.join(".local/bin");
        Ok(Self {
            installs: state.join("installs"),
            channels: state.join("channels"),
            staging: state.join("staging"),
            active: state.join("active"),
            lock: state.join("operation.lock"),
            transaction: state.join("pointer.transaction"),
            nvim_link: local_bin.join("nvim"),
            local_bin,
            state,
        })
    }

    fn channel_dir(&self, channel: Channel) -> PathBuf {
        self.channels.join(channel.as_str())
    }

    fn channel_link(&self, channel: Channel, name: &str) -> PathBuf {
        self.channel_dir(channel).join(name)
    }

    fn install_dir(&self, channel: Channel, release: &str) -> PathBuf {
        self.installs.join(format!("{channel}-{release}"))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct InstallMetadata {
    channel: Channel,
    release: String,
    version: String,
    asset: String,
    sha256: String,
}

impl InstallMetadata {
    fn parse(contents: &str, path: &Path) -> Result<Self> {
        let mut fields = BTreeMap::new();
        for (index, line) in contents.lines().enumerate() {
            if line.is_empty() {
                return Err(error(format!(
                    "malformed metadata at {}: empty line {}",
                    path.display(),
                    index + 1
                )));
            }
            let (key, value) = line.split_once('=').ok_or_else(|| {
                error(format!(
                    "malformed metadata at {} line {}: expected key=value",
                    path.display(),
                    index + 1
                ))
            })?;
            if !matches!(key, "channel" | "release" | "version" | "asset" | "sha256") {
                return Err(error(format!(
                    "unknown metadata field '{key}' at {}",
                    path.display()
                )));
            }
            if fields.insert(key, value).is_some() {
                return Err(error(format!(
                    "duplicate metadata field '{key}' at {}",
                    path.display()
                )));
            }
        }
        for key in ["asset", "channel", "release", "sha256", "version"] {
            if !fields.contains_key(key) {
                return Err(error(format!(
                    "missing metadata field '{key}' at {}",
                    path.display()
                )));
            }
        }

        let channel = Channel::parse(OsStr::new(fields["channel"]))?;
        validate_release_id(fields["release"])?;
        validate_version(fields["version"], path)?;
        if fields["asset"] != ASSET_NAME {
            return Err(error(format!(
                "unexpected asset '{}' in {}; expected {ASSET_NAME}",
                fields["asset"],
                path.display()
            )));
        }
        validate_sha256(fields["sha256"])?;
        Ok(Self {
            channel,
            release: fields["release"].to_owned(),
            version: fields["version"].to_owned(),
            asset: fields["asset"].to_owned(),
            sha256: fields["sha256"].to_owned(),
        })
    }

    fn serialize(&self) -> String {
        format!(
            "channel={}\nrelease={}\nversion={}\nasset={}\nsha256={}\n",
            self.channel, self.release, self.version, self.asset, self.sha256
        )
    }
}

#[derive(Clone, Debug)]
struct Installation {
    directory_name: String,
    path: PathBuf,
    metadata: InstallMetadata,
}

#[derive(Debug)]
struct ChannelState {
    current: Option<Installation>,
    previous: Option<Installation>,
}

#[derive(Debug)]
struct RemoteRelease {
    id: String,
    url: String,
    sha256: String,
    size: u64,
}

#[derive(Debug, Eq, PartialEq)]
struct PointerTransaction {
    channel: Channel,
    current: String,
    previous: Option<String>,
}

impl PointerTransaction {
    fn parse(contents: &str, path: &Path) -> Result<Self> {
        let lines: Vec<&str> = contents.lines().collect();
        if lines.len() != 3 {
            return Err(error(format!(
                "malformed pointer transaction at {}: expected exactly 3 lines",
                path.display()
            )));
        }
        let channel = Channel::parse(OsStr::new(lines[0]))?;
        validate_transaction_install(lines[1], channel, path)?;
        let previous = match lines[2] {
            "none" => None,
            value => {
                validate_transaction_install(value, channel, path)?;
                Some(value.to_owned())
            }
        };
        if previous.as_deref() == Some(lines[1]) {
            return Err(error(format!(
                "malformed pointer transaction at {}: current and previous are identical",
                path.display()
            )));
        }
        Ok(Self {
            channel,
            current: lines[1].to_owned(),
            previous,
        })
    }

    fn serialize(&self) -> String {
        format!(
            "{}\n{}\n{}\n",
            self.channel,
            self.current,
            self.previous.as_deref().unwrap_or("none")
        )
    }
}

struct LockGuard {
    path: PathBuf,
    held: bool,
}

impl LockGuard {
    fn acquire(paths: &Paths) -> Result<Self> {
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&paths.lock)
        {
            Ok(file) => file,
            Err(cause) if cause.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(error(format!(
                    "another nv operation is active, or a previous operation was interrupted: {} exists; once no nv process is running, inspect it and move it to the trash manually",
                    paths.lock.display()
                )));
            }
            Err(cause) => {
                return Err(error(format!(
                    "failed to acquire operation lock {}: {cause}",
                    paths.lock.display()
                )));
            }
        };
        if let Err(cause) = writeln!(file, "pid={}", std::process::id()) {
            let _ = fs::remove_file(&paths.lock);
            return Err(error(format!(
                "failed to write operation lock {}: {cause}",
                paths.lock.display()
            )));
        }
        if let Err(cause) = file.sync_all() {
            let _ = fs::remove_file(&paths.lock);
            return Err(error(format!(
                "failed to flush operation lock {}: {cause}",
                paths.lock.display()
            )));
        }
        Ok(Self {
            path: paths.lock.clone(),
            held: true,
        })
    }

    fn release(mut self) -> Result<()> {
        fs::remove_file(&self.path).map_err(|cause| {
            error(format!(
                "operation completed but failed to remove lock {}: {cause}",
                self.path.display()
            ))
        })?;
        self.held = false;
        Ok(())
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if self.held {
            let _ = fs::remove_file(&self.path);
        }
    }
}

struct StagingGuard {
    path: PathBuf,
    active: bool,
}

impl StagingGuard {
    fn create(paths: &Paths, channel: Channel) -> Result<Self> {
        if fs::read_dir(&paths.staging)
            .map_err(|cause| {
                error(format!(
                    "failed to inspect staging directory {}: {cause}",
                    paths.staging.display()
                ))
            })?
            .next()
            .is_some()
        {
            return Err(error(format!(
                "staging directory {} is not empty; inspect its contents and move stale data to the trash manually",
                paths.staging.display()
            )));
        }
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|cause| error(format!("system clock is before Unix epoch: {cause}")))?
            .as_nanos();
        let path = paths
            .staging
            .join(format!("{}-{}-{timestamp}", channel, std::process::id()));
        fs::create_dir(&path).map_err(|cause| {
            error(format!(
                "failed to create staging directory {}: {cause}",
                path.display()
            ))
        })?;
        Ok(Self { path, active: true })
    }

    fn finish(mut self) -> Result<()> {
        remove_staging_directory(&self.path)?;
        self.active = false;
        Ok(())
    }
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = remove_staging_directory(&self.path);
        }
    }
}

fn main() {
    if let Err(cause) = run() {
        eprintln!("nv: {cause}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse(env::args_os().skip(1))?;
    if cli == Cli::Help {
        println!("{USAGE}");
        return Ok(());
    }
    validate_platform()?;
    let paths = Paths::discover()?;
    match cli {
        Cli::Install(channel) => {
            preflight_external_programs()?;
            prepare_state_root(&paths)?;
            with_lock(&paths, || {
                ensure_layout(&paths)?;
                install_channel(&paths, channel)
            })
        }
        Cli::Use(channel) => {
            preflight_external_programs()?;
            prepare_state_root(&paths)?;
            with_lock(&paths, || {
                ensure_layout(&paths)?;
                validate_activation_destination(&paths)?;
                install_channel(&paths, channel)?;
                activate_channel(&paths, channel)
            })
        }
        Cli::Update(selection) => {
            preflight_external_programs()?;
            require_state_root(&paths)?;
            with_lock(&paths, || {
                validate_layout(&paths)?;
                update_installed(&paths, selection)
            })
        }
        Cli::Remove(selection) => {
            require_state_root(&paths)?;
            with_lock(&paths, || {
                validate_layout(&paths)?;
                remove_installed(&paths, selection)
            })
        }
        Cli::Rollback(channel) => {
            require_state_root(&paths)?;
            with_lock(&paths, || {
                validate_layout(&paths)?;
                rollback_channel(&paths, channel)
            })
        }
        Cli::Status => status(&paths),
        Cli::Help => unreachable!(),
    }
}

fn validate_platform() -> Result<()> {
    if env::consts::OS != "linux" || env::consts::ARCH != "x86_64" {
        return Err(error(format!(
            "unsupported platform {} {}; nv supports only Linux x86_64",
            env::consts::OS,
            env::consts::ARCH
        )));
    }
    Ok(())
}

fn preflight_external_programs() -> Result<()> {
    for program in ["curl", "jq", "sha256sum", "tar"] {
        let output = Command::new(program).arg("--version").output().map_err(|cause| {
            error(format!(
                "required program '{program}' is unavailable while checking its version: {cause}"
            ))
        })?;
        if !output.status.success() {
            return Err(command_failure(program, "version check", &output, None));
        }
    }
    Ok(())
}

fn prepare_state_root(paths: &Paths) -> Result<()> {
    match fs::symlink_metadata(&paths.state) {
        Ok(metadata) => require_secure_directory(&paths.state, &metadata),
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => {
            create_private_dir(&paths.state)
        }
        Err(cause) => Err(error(format!(
            "failed to inspect state directory {}: {cause}",
            paths.state.display()
        ))),
    }
}

fn require_state_root(paths: &Paths) -> Result<()> {
    let metadata = fs::symlink_metadata(&paths.state).map_err(|cause| {
        if cause.kind() == std::io::ErrorKind::NotFound {
            error(format!(
                "nv state directory {} does not exist; install a channel first",
                paths.state.display()
            ))
        } else {
            error(format!(
                "failed to inspect state directory {}: {cause}",
                paths.state.display()
            ))
        }
    })?;
    require_secure_directory(&paths.state, &metadata)
}

fn with_lock<T>(paths: &Paths, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let lock = LockGuard::acquire(paths)?;
    let result = recover_pointer_transaction(paths).and_then(|()| operation());
    let unlock = lock.release();
    match (result, unlock) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(cause), Ok(())) => Err(cause),
        (Ok(_), Err(cause)) => Err(cause),
        (Err(operation_cause), Err(unlock_cause)) => Err(error(format!(
            "{operation_cause}; additionally, {unlock_cause}"
        ))),
    }
}

fn layout_directories(paths: &Paths) -> Vec<PathBuf> {
    vec![
        paths.installs.clone(),
        paths.channels.clone(),
        paths.staging.clone(),
        paths.channel_dir(Channel::Stable),
        paths.channel_dir(Channel::Nightly),
    ]
}

fn ensure_layout(paths: &Paths) -> Result<()> {
    let required = layout_directories(paths);
    let existing = required
        .iter()
        .filter(|path| fs::symlink_metadata(path).is_ok())
        .count();
    if existing == 0 {
        for path in required {
            create_private_dir(&path)?;
        }
        return Ok(());
    }
    validate_layout(paths)
}

fn validate_layout(paths: &Paths) -> Result<()> {
    for path in layout_directories(paths) {
        let metadata = fs::symlink_metadata(&path).map_err(|cause| {
            error(format!(
                "required state directory {} is missing or inaccessible: {cause}",
                path.display()
            ))
        })?;
        require_real_directory(&path, &metadata)?;
    }
    Ok(())
}

fn require_real_directory(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(error(format!(
            "expected a real directory at {}, found another file type",
            path.display()
        )));
    }
    Ok(())
}

fn require_secure_directory(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    require_real_directory(path, metadata)?;
    if metadata.permissions().mode() & 0o022 != 0 {
        return Err(error(format!(
            "refusing writable-by-group-or-others directory {}; remove group/other write permissions",
            path.display()
        )));
    }
    Ok(())
}

fn create_private_dir(path: &Path) -> Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(path).map_err(|cause| {
        error(format!(
            "failed to create private directory {}: {cause}",
            path.display()
        ))
    })
}

fn install_channel(paths: &Paths, channel: Channel) -> Result<()> {
    let state = read_channel_state(paths, channel)?;
    let staging = StagingGuard::create(paths, channel)?;
    let release = resolve_release(channel, &staging.path)?;
    if let Some(current) = &state.current
        && current.metadata.release == release.id
        && current.metadata.sha256 == release.sha256
    {
        println!(
            "{channel} is already current: {} (release {})",
            current.metadata.version, current.metadata.release
        );
        staging.finish()?;
        return Ok(());
    }

    let final_path = paths.install_dir(channel, &release.id);
    let metadata = match fs::symlink_metadata(&final_path) {
        Ok(_) => {
            let metadata = read_installation(paths, &final_path, Some(channel))?;
            if metadata.release != release.id || metadata.sha256 != release.sha256 {
                return Err(error(format!(
                    "immutable installation path {} conflicts with release {}; inspect it manually",
                    final_path.display(),
                    release.id
                )));
            }
            metadata
        }
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => {
            let archive = staging.path.join(format!("{ASSET_NAME}.part"));
            download_asset(&release, &archive)?;
            verify_size(&archive, release.size)?;
            verify_archive_digest(&archive, &release.sha256)?;

            let extraction = staging.path.join("extracted");
            fs::create_dir(&extraction).map_err(|cause| {
                error(format!(
                    "failed to create extraction directory {}: {cause}",
                    extraction.display()
                ))
            })?;
            extract_archive(&archive, &extraction)?;
            let installation = validate_extracted_layout(&extraction)?;
            let version = staged_version(&installation)?;
            let metadata = InstallMetadata {
                channel,
                release: release.id.clone(),
                version,
                asset: ASSET_NAME.to_owned(),
                sha256: release.sha256,
            };
            write_metadata(&installation.join(".nv-metadata"), &metadata)?;
            sync_directory(&installation)?;
            fs::rename(&installation, &final_path).map_err(|cause| {
                error(format!(
                    "failed to publish staged installation {} as {}: {cause}",
                    installation.display(),
                    final_path.display()
                ))
            })?;
            sync_directory(&paths.installs)?;
            metadata
        }
        Err(cause) => {
            return Err(error(format!(
                "failed to inspect immutable installation path {}: {cause}",
                final_path.display()
            )));
        }
    };
    replace_channel_pointers(paths, channel, state.current.as_ref(), &metadata)?;
    if let Err(cause) = cleanup_unreferenced_installs(paths) {
        eprintln!("nv: warning: installation committed, but cleanup failed: {cause}");
    }
    if let Err(cause) = staging.finish() {
        eprintln!("nv: warning: installation committed, but staging cleanup failed: {cause}");
    }
    println!(
        "installed {channel} {} (release {})",
        metadata.version, metadata.release
    );
    Ok(())
}

fn resolve_release(channel: Channel, staging: &Path) -> Result<RemoteRelease> {
    let response_path = staging.join("release.json");
    curl_to_file(
        channel.api_url(),
        &response_path,
        true,
        MAX_API_RESPONSE_SIZE,
        API_TIMEOUT_SECONDS,
    )?;
    let filter = r#"
      (.assets // error("missing assets")) as $assets
      | ($assets | map(select(.name == $asset))) as $matches
      | if ($matches | length) != 1 then error("expected exactly one asset named '\($asset)'; Neovim may have renamed its Linux x86_64 release asset") else . end
      | ($matches[0]) as $asset_data
      | (.id | if type == "number" and . > 0 and . == floor then tostring else error("invalid release id") end),
        ($asset_data.browser_download_url | if type == "string" and length > 0 then . else error("invalid browser_download_url") end),
        ($asset_data.digest | if type == "string" and length > 0 then . else error("invalid asset digest") end),
        ($asset_data.size | if type == "number" and . > 0 and . == floor then tostring else error("invalid asset size") end)
    "#;
    let output = Command::new("jq")
        .arg("--raw-output")
        .arg("--exit-status")
        .arg("--arg")
        .arg("asset")
        .arg(ASSET_NAME)
        .arg(filter)
        .arg(&response_path)
        .output()
        .map_err(|cause| {
            error(format!(
                "failed to run jq for GitHub release metadata {}: {cause}",
                response_path.display()
            ))
        })?;
    if !output.status.success() {
        return Err(command_failure(
            "jq",
            "strict GitHub release metadata extraction",
            &output,
            Some(&response_path),
        ));
    }
    let stdout = std::str::from_utf8(&output.stdout).map_err(|cause| {
        error(format!(
            "jq produced non-UTF-8 release metadata for {}: {cause}",
            response_path.display()
        ))
    })?;
    let lines: Vec<&str> = stdout.lines().collect();
    if lines.len() != 4 {
        return Err(error(format!(
            "jq produced {} release metadata fields for {}, expected exactly 4",
            lines.len(),
            response_path.display()
        )));
    }
    validate_release_id(lines[0])?;
    validate_download_url(lines[1])?;
    let sha256 = parse_api_digest(lines[2])?;
    let size = lines[3].parse::<u64>().map_err(|cause| {
        error(format!(
            "invalid asset size '{}' from {}: {cause}",
            lines[3],
            response_path.display()
        ))
    })?;
    if size == 0 {
        return Err(error(format!(
            "asset size from {} must be greater than zero",
            response_path.display()
        )));
    }
    if size > MAX_ASSET_SIZE {
        return Err(error(format!(
            "GitHub reports an asset size of {size} bytes from {}, exceeding nv's {MAX_ASSET_SIZE} byte safety limit; raise MAX_ASSET_SIZE if this is an expected Neovim release size",
            response_path.display(),
        )));
    }
    Ok(RemoteRelease {
        id: lines[0].to_owned(),
        url: lines[1].to_owned(),
        sha256,
        size,
    })
}

fn curl_to_file(
    url: &str,
    destination: &Path,
    api_request: bool,
    max_size: u64,
    timeout_seconds: u64,
) -> Result<()> {
    let mut command = Command::new("curl");
    command
        .arg("--show-error")
        .arg("--fail-with-body")
        .arg("--location")
        .arg("--proto")
        .arg("=https")
        .arg("--proto-redir")
        .arg("=https")
        .arg("--connect-timeout")
        .arg("15")
        .arg("--max-time")
        .arg(timeout_seconds.to_string())
        .arg("--max-filesize")
        .arg(max_size.to_string())
        .arg("--header")
        .arg(concat!("User-Agent: nv/", env!("CARGO_PKG_VERSION")))
        .arg("--output")
        .arg(destination);
    if !api_request && std::io::stderr().is_terminal() {
        command.arg("--progress-bar").stderr(Stdio::inherit());
    } else {
        command.arg("--silent");
    }
    if api_request {
        command
            .arg("--header")
            .arg("Accept: application/vnd.github+json")
            .arg("--header")
            .arg(format!("X-GitHub-Api-Version: {GITHUB_API_VERSION}"));
    }
    command.arg(url);
    let output = command.output().map_err(|cause| {
        error(format!(
            "failed to run curl for {url} into {}: {cause}",
            destination.display()
        ))
    })?;
    if output.status.success() {
        return Ok(());
    }
    let mut failure = command_failure("curl", "HTTPS download", &output, Some(destination)).0;
    if let Ok(mut file) = File::open(destination) {
        let mut body = String::new();
        let _ = Read::by_ref(&mut file).take(4096).read_to_string(&mut body);
        let body = body.trim();
        if !body.is_empty() {
            failure.push_str(&format!("; response body: {body}"));
        }
    }
    Err(error(failure))
}

fn download_asset(release: &RemoteRelease, destination: &Path) -> Result<()> {
    curl_to_file(
        &release.url,
        destination,
        false,
        release.size,
        ASSET_TIMEOUT_SECONDS,
    )
}

fn verify_size(path: &Path, expected: u64) -> Result<()> {
    let actual = fs::metadata(path)
        .map_err(|cause| {
            error(format!(
                "failed to inspect downloaded archive {}: {cause}",
                path.display()
            ))
        })?
        .len();
    if actual != expected {
        return Err(error(format!(
            "downloaded archive size mismatch at {}: expected {expected} bytes, got {actual}",
            path.display()
        )));
    }
    Ok(())
}

fn verify_archive_digest(path: &Path, expected: &str) -> Result<()> {
    let output = Command::new("sha256sum")
        .arg("--binary")
        .arg(path)
        .output()
        .map_err(|cause| {
            error(format!(
                "failed to run sha256sum for {}: {cause}",
                path.display()
            ))
        })?;
    if !output.status.success() {
        return Err(command_failure(
            "sha256sum",
            "archive verification",
            &output,
            Some(path),
        ));
    }
    let stdout = std::str::from_utf8(&output.stdout).map_err(|cause| {
        error(format!(
            "sha256sum produced non-UTF-8 output for {}: {cause}",
            path.display()
        ))
    })?;
    let lines: Vec<&str> = stdout.lines().collect();
    if lines.len() != 1 {
        return Err(error(format!(
            "sha256sum produced {} lines for {}, expected exactly one",
            lines.len(),
            path.display()
        )));
    }
    let actual = lines[0].split_whitespace().next().ok_or_else(|| {
        error(format!(
            "sha256sum produced empty output for {}",
            path.display()
        ))
    })?;
    validate_sha256(actual)?;
    if actual != expected {
        return Err(error(format!(
            "SHA-256 mismatch for {}: expected {expected}, got {actual}",
            path.display()
        )));
    }
    Ok(())
}

fn extract_archive(archive: &Path, destination: &Path) -> Result<()> {
    let output = Command::new("tar")
        .arg("--extract")
        .arg("--gzip")
        .arg("--file")
        .arg(archive)
        .arg("--directory")
        .arg(destination)
        .arg("--no-same-owner")
        .arg("--no-same-permissions")
        .output()
        .map_err(|cause| {
            error(format!(
                "failed to run tar for {} into {}: {cause}",
                archive.display(),
                destination.display()
            ))
        })?;
    if !output.status.success() {
        return Err(command_failure(
            "tar",
            "archive extraction",
            &output,
            Some(archive),
        ));
    }
    Ok(())
}

fn validate_extracted_layout(extraction: &Path) -> Result<PathBuf> {
    let entries = fs::read_dir(extraction)
        .map_err(|cause| {
            error(format!(
                "failed to inspect extracted archive {}: {cause}",
                extraction.display()
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|cause| {
            error(format!(
                "failed to read an entry in extracted archive {}: {cause}",
                extraction.display()
            ))
        })?;
    if entries.len() != 1 || entries[0].file_name() != OsStr::new(ARCHIVE_ROOT) {
        return Err(error(format!(
            "unexpected archive layout in {}: expected exactly one top-level directory named {ARCHIVE_ROOT}",
            extraction.display()
        )));
    }
    let installation = entries[0].path();
    let metadata = fs::symlink_metadata(&installation).map_err(|cause| {
        error(format!(
            "failed to inspect extracted installation {}: {cause}",
            installation.display()
        ))
    })?;
    require_real_directory(&installation, &metadata)?;
    validate_nvim_binary(&installation.join("bin/nvim"))?;
    Ok(installation)
}

fn validate_nvim_binary(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|cause| {
        error(format!(
            "required Neovim executable {} is missing or inaccessible: {cause}",
            path.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(error(format!(
            "required Neovim executable {} is not a regular file",
            path.display()
        )));
    }
    if metadata.permissions().mode() & 0o111 == 0 {
        return Err(error(format!(
            "required Neovim executable {} is not executable",
            path.display()
        )));
    }
    Ok(())
}

fn staged_version(installation: &Path) -> Result<String> {
    let executable = installation.join("bin/nvim");
    let output = Command::new(&executable)
        .arg("--version")
        .output()
        .map_err(|cause| {
            error(format!(
                "failed to run staged Neovim {} --version: {cause}",
                executable.display()
            ))
        })?;
    if !output.status.success() {
        return Err(command_failure(
            &executable.display().to_string(),
            "staged Neovim version check",
            &output,
            Some(&executable),
        ));
    }
    let stdout = std::str::from_utf8(&output.stdout).map_err(|cause| {
        error(format!(
            "staged Neovim {} produced non-UTF-8 version output: {cause}",
            executable.display()
        ))
    })?;
    let version = stdout.lines().next().ok_or_else(|| {
        error(format!(
            "staged Neovim {} produced no version",
            executable.display()
        ))
    })?;
    validate_version(version, &executable)?;
    Ok(version.to_owned())
}

fn write_metadata(path: &Path, metadata: &InstallMetadata) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|cause| {
            error(format!(
                "failed to create installation metadata {}: {cause}",
                path.display()
            ))
        })?;
    file.write_all(metadata.serialize().as_bytes())
        .map_err(|cause| {
            error(format!(
                "failed to write installation metadata {}: {cause}",
                path.display()
            ))
        })?;
    file.flush().map_err(|cause| {
        error(format!(
            "failed to flush installation metadata {}: {cause}",
            path.display()
        ))
    })?;
    file.sync_all().map_err(|cause| {
        error(format!(
            "failed to sync installation metadata {}: {cause}",
            path.display()
        ))
    })
}

fn replace_channel_pointers(
    paths: &Paths,
    channel: Channel,
    old_current: Option<&Installation>,
    new_metadata: &InstallMetadata,
) -> Result<()> {
    commit_pointer_transaction(
        paths,
        &PointerTransaction {
            channel,
            current: format!("{channel}-{}", new_metadata.release),
            previous: old_current.map(|installation| installation.directory_name.clone()),
        },
    )
}

fn channel_relative_target(directory_name: &str) -> PathBuf {
    PathBuf::from("../../installs").join(directory_name)
}

fn atomic_symlink_replace(link: &Path, target: &Path) -> Result<()> {
    let name = link
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| error(format!("invalid symlink path {}", link.display())))?;
    let temporary = link.with_file_name(format!(".{name}.nv-new"));
    match fs::symlink_metadata(&temporary) {
        Ok(metadata) => {
            if !metadata.file_type().is_symlink()
                || fs::read_link(&temporary).map_err(|cause| {
                    error(format!(
                        "failed to read temporary symlink {}: {cause}",
                        temporary.display()
                    ))
                })? != target
            {
                return Err(error(format!(
                    "temporary link {} does not match the pending transaction",
                    temporary.display()
                )));
            }
        }
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => {
            symlink(target, &temporary).map_err(|cause| {
                error(format!(
                    "failed to create temporary symlink {} -> {}: {cause}",
                    temporary.display(),
                    target.display()
                ))
            })?;
        }
        Err(cause) => {
            return Err(error(format!(
                "failed to inspect temporary symlink {}: {cause}",
                temporary.display()
            )));
        }
    }
    if let Err(cause) = fs::rename(&temporary, link) {
        let _ = fs::remove_file(&temporary);
        return Err(error(format!(
            "failed to atomically replace symlink {} -> {}: {cause}",
            link.display(),
            target.display()
        )));
    }
    Ok(())
}

fn commit_pointer_transaction(paths: &Paths, transaction: &PointerTransaction) -> Result<()> {
    let temporary = paths
        .transaction
        .with_file_name(".pointer.transaction.nv-new");
    if fs::symlink_metadata(&paths.transaction).is_ok() || fs::symlink_metadata(&temporary).is_ok()
    {
        return Err(error(format!(
            "pointer transaction state already exists under {}; inspect it manually",
            paths.state.display()
        )));
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|cause| {
            error(format!(
                "failed to create pointer transaction {}: {cause}",
                temporary.display()
            ))
        })?;
    if let Err(cause) = file
        .write_all(transaction.serialize().as_bytes())
        .and_then(|()| file.sync_all())
    {
        let _ = fs::remove_file(&temporary);
        return Err(error(format!(
            "failed to persist pointer transaction {}: {cause}",
            temporary.display()
        )));
    }
    fs::rename(&temporary, &paths.transaction).map_err(|cause| {
        error(format!(
            "failed to publish pointer transaction {}: {cause}",
            paths.transaction.display()
        ))
    })?;
    sync_directory(&paths.state)?;
    finish_pointer_transaction(paths, transaction)
}

fn recover_pointer_transaction(paths: &Paths) -> Result<()> {
    let temporary = paths
        .transaction
        .with_file_name(".pointer.transaction.nv-new");
    let transaction_metadata = match fs::symlink_metadata(&paths.transaction) {
        Ok(metadata) => Some(metadata),
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => None,
        Err(cause) => {
            return Err(error(format!(
                "failed to inspect pointer transaction {}: {cause}",
                paths.transaction.display()
            )));
        }
    };
    if transaction_metadata.is_none() {
        if fs::symlink_metadata(&temporary).is_ok() {
            fs::remove_file(&temporary).map_err(|cause| {
                error(format!(
                    "failed to remove unpublished pointer transaction {}: {cause}",
                    temporary.display()
                ))
            })?;
            sync_directory(&paths.state)?;
        }
        return Ok(());
    }
    let metadata = transaction_metadata.expect("checked above");
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(error(format!(
            "pointer transaction {} is not a regular file",
            paths.transaction.display()
        )));
    }
    if fs::symlink_metadata(&temporary).is_ok() {
        fs::remove_file(&temporary).map_err(|cause| {
            error(format!(
                "failed to remove stale pointer transaction {}: {cause}",
                temporary.display()
            ))
        })?;
    }
    let contents = fs::read_to_string(&paths.transaction).map_err(|cause| {
        error(format!(
            "failed to read pointer transaction {}: {cause}",
            paths.transaction.display()
        ))
    })?;
    let transaction = PointerTransaction::parse(&contents, &paths.transaction)?;
    finish_pointer_transaction(paths, &transaction)
}

fn finish_pointer_transaction(paths: &Paths, transaction: &PointerTransaction) -> Result<()> {
    let current_path = paths.installs.join(&transaction.current);
    read_installation(paths, &current_path, Some(transaction.channel))?;
    if let Some(previous) = &transaction.previous {
        read_installation(
            paths,
            &paths.installs.join(previous),
            Some(transaction.channel),
        )?;
        atomic_symlink_replace(
            &paths.channel_link(transaction.channel, "previous"),
            &channel_relative_target(previous),
        )?;
    } else if fs::symlink_metadata(paths.channel_link(transaction.channel, "previous")).is_ok() {
        return Err(error(format!(
            "pointer transaction expected no previous {} installation",
            transaction.channel
        )));
    }
    atomic_symlink_replace(
        &paths.channel_link(transaction.channel, "current"),
        &channel_relative_target(&transaction.current),
    )?;
    read_channel_state(paths, transaction.channel)?;
    sync_directory(&paths.channel_dir(transaction.channel))?;
    fs::remove_file(&paths.transaction).map_err(|cause| {
        error(format!(
            "pointer transaction completed but failed to remove {}: {cause}",
            paths.transaction.display()
        ))
    })?;
    sync_directory(&paths.state)
}

fn read_channel_state(paths: &Paths, channel: Channel) -> Result<ChannelState> {
    let current = read_channel_pointer(paths, channel, "current")?;
    let previous = read_channel_pointer(paths, channel, "previous")?;
    if current.is_none() && previous.is_some() {
        return Err(error(format!(
            "malformed {channel} channel state at {}: previous exists without current",
            paths.channel_dir(channel).display()
        )));
    }
    if let (Some(current), Some(previous)) = (&current, &previous)
        && current.directory_name == previous.directory_name
    {
        return Err(error(format!(
            "malformed {channel} channel state at {}: current and previous point to {}",
            paths.channel_dir(channel).display(),
            current.path.display()
        )));
    }
    Ok(ChannelState { current, previous })
}

fn read_all_channel_states(paths: &Paths) -> Result<Vec<(Channel, ChannelState)>> {
    Channel::ALL
        .into_iter()
        .map(|channel| Ok((channel, read_channel_state(paths, channel)?)))
        .collect()
}

fn read_channel_pointer(
    paths: &Paths,
    channel: Channel,
    name: &str,
) -> Result<Option<Installation>> {
    let link = paths.channel_link(channel, name);
    let link_metadata = match fs::symlink_metadata(&link) {
        Ok(metadata) => metadata,
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(cause) => {
            return Err(error(format!(
                "failed to inspect channel pointer {}: {cause}",
                link.display()
            )));
        }
    };
    if !link_metadata.file_type().is_symlink() {
        return Err(error(format!(
            "channel pointer {} is not a symlink",
            link.display()
        )));
    }
    let target = fs::read_link(&link).map_err(|cause| {
        error(format!(
            "failed to read channel pointer {}: {cause}",
            link.display()
        ))
    })?;
    let directory_name = validate_channel_target(&target, channel, &link)?;
    let path = paths.installs.join(&directory_name);
    let metadata = read_installation(paths, &path, Some(channel))?;
    Ok(Some(Installation {
        directory_name,
        path,
        metadata,
    }))
}

fn validate_channel_target(target: &Path, channel: Channel, link: &Path) -> Result<String> {
    let components: Vec<Component<'_>> = target.components().collect();
    if components.len() != 4
        || components[0] != Component::ParentDir
        || components[1] != Component::ParentDir
        || components[2] != Component::Normal(OsStr::new("installs"))
    {
        return Err(error(format!(
            "channel pointer {} has unmanaged target {}; expected ../../installs/{}-<release-id>",
            link.display(),
            target.display(),
            channel
        )));
    }
    let name = match components[3] {
        Component::Normal(name) => name.to_str().ok_or_else(|| {
            error(format!(
                "channel pointer {} target is not valid UTF-8: {}",
                link.display(),
                target.display()
            ))
        })?,
        _ => {
            return Err(error(format!(
                "channel pointer {} has unmanaged target {}",
                link.display(),
                target.display()
            )));
        }
    };
    let (target_channel, _) = parse_install_name(name)?;
    if target_channel != channel {
        return Err(error(format!(
            "channel pointer {} targets {target_channel} installation {name}",
            link.display()
        )));
    }
    Ok(name.to_owned())
}

fn read_installation(
    paths: &Paths,
    path: &Path,
    expected_channel: Option<Channel>,
) -> Result<InstallMetadata> {
    if path.parent() != Some(paths.installs.as_path()) {
        return Err(error(format!(
            "installation path {} is outside managed directory {}",
            path.display(),
            paths.installs.display()
        )));
    }
    let name = path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| error(format!("invalid installation path {}", path.display())))?;
    let (name_channel, name_release) = parse_install_name(name)?;
    if let Some(expected) = expected_channel
        && name_channel != expected
    {
        return Err(error(format!(
            "installation {} belongs to {name_channel}, expected {expected}",
            path.display()
        )));
    }
    let directory_metadata = fs::symlink_metadata(path).map_err(|cause| {
        error(format!(
            "managed installation {} is missing or inaccessible: {cause}",
            path.display()
        ))
    })?;
    require_real_directory(path, &directory_metadata)?;
    validate_nvim_binary(&path.join("bin/nvim"))?;
    let metadata_path = path.join(".nv-metadata");
    let contents = fs::read_to_string(&metadata_path).map_err(|cause| {
        error(format!(
            "failed to read installation metadata {}: {cause}",
            metadata_path.display()
        ))
    })?;
    let metadata = InstallMetadata::parse(&contents, &metadata_path)?;
    if metadata.channel != name_channel || metadata.release != name_release {
        return Err(error(format!(
            "metadata {} does not match installation directory {name}",
            metadata_path.display()
        )));
    }
    Ok(metadata)
}

fn parse_install_name(name: &str) -> Result<(Channel, &str)> {
    for channel in Channel::ALL {
        if let Some(release) = name.strip_prefix(&format!("{channel}-")) {
            validate_release_id(release)?;
            return Ok((channel, release));
        }
    }
    Err(error(format!(
        "unmanaged installation name '{name}'; expected stable-<release-id> or nightly-<release-id>"
    )))
}

fn validate_transaction_install(name: &str, expected_channel: Channel, path: &Path) -> Result<()> {
    let (channel, _) = parse_install_name(name)?;
    if channel != expected_channel {
        return Err(error(format!(
            "pointer transaction {} references {channel} installation {name}, expected {expected_channel}",
            path.display()
        )));
    }
    Ok(())
}

fn update_installed(paths: &Paths, selection: ChannelSelection) -> Result<()> {
    match selection {
        ChannelSelection::Channel(channel) => {
            if read_channel_state(paths, channel)?.current.is_none() {
                return Err(error(format!(
                    "{channel} is not installed; run 'nv install {channel}' first"
                )));
            }
            install_channel(paths, channel)
        }
        ChannelSelection::All => {
            let installed: Vec<Channel> = read_all_channel_states(paths)?
                .into_iter()
                .filter_map(|(channel, state)| state.current.is_some().then_some(channel))
                .collect();
            if installed.is_empty() {
                return Err(error(
                    "no channels are installed; run 'nv install stable' or 'nv install nightly' first",
                ));
            }
            for channel in installed {
                install_channel(paths, channel)?;
            }
            Ok(())
        }
    }
}

fn remove_installed(paths: &Paths, selection: ChannelSelection) -> Result<()> {
    let removals = match selection {
        ChannelSelection::Channel(channel) => {
            let state = read_channel_state(paths, channel)?;
            if state.current.is_none() {
                return Err(error(format!("{channel} is not installed")));
            }
            vec![(channel, state)]
        }
        ChannelSelection::All => {
            let removals: Vec<(Channel, ChannelState)> = read_all_channel_states(paths)?
                .into_iter()
                .filter(|(_, state)| state.current.is_some())
                .collect();
            if removals.is_empty() {
                return Err(error("no channels are installed"));
            }
            removals
        }
    };
    let active = validate_active_channel(paths)?;
    if active.is_some_and(|active| removals.iter().any(|(channel, _)| *channel == active)) {
        validate_nvim_exposure(paths)?;
        deactivate(paths)?;
    }
    for (channel, state) in &removals {
        if state.previous.is_some() {
            remove_channel_pointer(paths, *channel, "previous")?;
        }
        remove_channel_pointer(paths, *channel, "current")?;
        sync_directory(&paths.channel_dir(*channel))?;
    }
    cleanup_unreferenced_installs(paths)?;
    for (channel, _) in removals {
        println!("removed {channel}");
    }
    Ok(())
}

fn deactivate(paths: &Paths) -> Result<()> {
    fs::remove_file(&paths.active).map_err(|cause| {
        error(format!(
            "failed to remove active channel link {}: {cause}",
            paths.active.display()
        ))
    })?;
    sync_directory(&paths.state)?;
    fs::remove_file(&paths.nvim_link).map_err(|cause| {
        error(format!(
            "failed to remove managed executable link {}: {cause}",
            paths.nvim_link.display()
        ))
    })?;
    sync_directory(&paths.local_bin)
}

fn remove_channel_pointer(paths: &Paths, channel: Channel, name: &str) -> Result<()> {
    let path = paths.channel_link(channel, name);
    fs::remove_file(&path).map_err(|cause| {
        error(format!(
            "failed to remove {channel} {name} pointer {}: {cause}",
            path.display()
        ))
    })
}

fn activate_channel(paths: &Paths, channel: Channel) -> Result<()> {
    let current = read_channel_state(paths, channel)?.current.ok_or_else(|| {
        error(format!(
            "cannot activate {channel}: channel is not installed"
        ))
    })?;
    validate_active_channel(paths)?;
    ensure_nvim_link(paths)?;
    let target = PathBuf::from("channels")
        .join(channel.as_str())
        .join("current");
    atomic_symlink_replace(&paths.active, &target)?;
    validate_active_channel(paths)?;
    sync_directory(&paths.state)?;
    println!(
        "using {channel} {} (release {})",
        current.metadata.version, current.metadata.release
    );
    Ok(())
}

fn ensure_nvim_link(paths: &Paths) -> Result<()> {
    match fs::symlink_metadata(&paths.local_bin) {
        Ok(metadata) => require_secure_directory(&paths.local_bin, &metadata)?,
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => {
            create_private_dir(&paths.local_bin)?;
        }
        Err(cause) => {
            return Err(error(format!(
                "failed to inspect executable directory {}: {cause}",
                paths.local_bin.display()
            )));
        }
    }
    match fs::symlink_metadata(&paths.nvim_link) {
        Ok(metadata) => validate_managed_nvim_link(paths, &metadata)?,
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => {
            atomic_symlink_replace(&paths.nvim_link, Path::new(NVIM_LINK_TARGET))?;
            sync_directory(&paths.local_bin)?;
        }
        Err(cause) => {
            return Err(error(format!(
                "failed to inspect executable link {}: {cause}",
                paths.nvim_link.display()
            )));
        }
    }
    Ok(())
}

fn validate_activation_destination(paths: &Paths) -> Result<()> {
    match fs::symlink_metadata(&paths.local_bin) {
        Ok(metadata) => require_secure_directory(&paths.local_bin, &metadata)?,
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(cause) => {
            return Err(error(format!(
                "failed to inspect executable directory {}: {cause}",
                paths.local_bin.display()
            )));
        }
    }
    match fs::symlink_metadata(&paths.nvim_link) {
        Ok(metadata) => validate_managed_nvim_link(paths, &metadata),
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(cause) => Err(error(format!(
            "failed to inspect executable link {}: {cause}",
            paths.nvim_link.display()
        ))),
    }
}

fn validate_managed_nvim_link(paths: &Paths, metadata: &fs::Metadata) -> Result<()> {
    if !metadata.file_type().is_symlink() {
        return Err(error(format!(
            "executable path {} is not nv's managed symlink",
            paths.nvim_link.display()
        )));
    }
    let target = fs::read_link(&paths.nvim_link).map_err(|cause| {
        error(format!(
            "failed to read executable symlink {}: {cause}",
            paths.nvim_link.display()
        ))
    })?;
    if target != Path::new(NVIM_LINK_TARGET) {
        return Err(error(format!(
            "executable symlink {} has unmanaged target {}",
            paths.nvim_link.display(),
            target.display()
        )));
    }
    Ok(())
}

fn validate_active_channel(paths: &Paths) -> Result<Option<Channel>> {
    let metadata = match fs::symlink_metadata(&paths.active) {
        Ok(metadata) => metadata,
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(cause) => {
            return Err(error(format!(
                "failed to inspect active channel link {}: {cause}",
                paths.active.display()
            )));
        }
    };
    if !metadata.file_type().is_symlink() {
        return Err(error(format!(
            "active channel path {} is not a symlink",
            paths.active.display()
        )));
    }
    let target = fs::read_link(&paths.active).map_err(|cause| {
        error(format!(
            "failed to read active channel link {}: {cause}",
            paths.active.display()
        ))
    })?;
    for channel in Channel::ALL {
        let expected = PathBuf::from("channels")
            .join(channel.as_str())
            .join("current");
        if target == expected {
            if read_channel_state(paths, channel)?.current.is_none() {
                return Err(error(format!(
                    "active channel link {} points to uninstalled {channel} channel",
                    paths.active.display()
                )));
            }
            return Ok(Some(channel));
        }
    }
    Err(error(format!(
        "active channel link {} has unmanaged target {}",
        paths.active.display(),
        target.display()
    )))
}

fn rollback_channel(paths: &Paths, channel: Channel) -> Result<()> {
    let state = read_channel_state(paths, channel)?;
    let current = state.current.ok_or_else(|| {
        error(format!(
            "cannot roll back {channel}: channel is not installed"
        ))
    })?;
    let previous = state.previous.ok_or_else(|| {
        error(format!(
            "cannot roll back {channel}: no previous installation exists"
        ))
    })?;
    commit_pointer_transaction(
        paths,
        &PointerTransaction {
            channel,
            current: previous.directory_name,
            previous: Some(current.directory_name),
        },
    )?;
    let new_current = read_channel_state(paths, channel)?.current.ok_or_else(|| {
        error(format!(
            "rollback left {channel} without a current installation"
        ))
    })?;
    sync_directory(&paths.channel_dir(channel))?;
    println!(
        "rolled back {channel} to {} (release {})",
        new_current.metadata.version, new_current.metadata.release
    );
    Ok(())
}

fn cleanup_unreferenced_installs(paths: &Paths) -> Result<()> {
    let mut referenced = BTreeSet::new();
    for (_, state) in read_all_channel_states(paths)? {
        for installation in [state.current, state.previous].into_iter().flatten() {
            referenced.insert(installation.directory_name);
        }
    }
    let entries = fs::read_dir(&paths.installs)
        .map_err(|cause| {
            error(format!(
                "failed to inspect installation directory {}: {cause}",
                paths.installs.display()
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|cause| {
            error(format!(
                "failed to read an installation entry in {}: {cause}",
                paths.installs.display()
            ))
        })?;
    let mut unreferenced = Vec::new();
    for entry in entries {
        let name = entry.file_name().into_string().map_err(|_| {
            error(format!(
                "non-UTF-8 entry in managed installation directory {}",
                paths.installs.display()
            ))
        })?;
        let path = entry.path();
        read_installation(paths, &path, None)?;
        if !referenced.contains(&name) {
            unreferenced.push(path);
        }
    }
    for path in unreferenced {
        fs::remove_dir_all(&path).map_err(|cause| {
            error(format!(
                "failed to remove unreferenced managed installation {}: {cause}",
                path.display()
            ))
        })?;
    }
    sync_directory(&paths.installs)
}

fn status(paths: &Paths) -> Result<()> {
    match fs::symlink_metadata(&paths.state) {
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => {
            print_empty_status();
            return Ok(());
        }
        Err(cause) => {
            return Err(error(format!(
                "failed to inspect state directory {}: {cause}",
                paths.state.display()
            )));
        }
        Ok(metadata) => require_secure_directory(&paths.state, &metadata)?,
    }
    with_lock(paths, || {
        validate_layout(paths)?;
        let active = validate_active_channel(paths)?;
        if active.is_some() {
            validate_nvim_exposure(paths)?;
        }
        println!(
            "active: {}",
            active.map_or_else(|| "none".to_owned(), |channel| channel.to_string())
        );
        for (channel, state) in read_all_channel_states(paths)? {
            print_status_entry(channel, "current", state.current.as_ref());
            print_status_entry(channel, "previous", state.previous.as_ref());
        }
        Ok(())
    })
}

fn print_empty_status() {
    println!("active: none");
    for channel in Channel::ALL {
        println!("{channel} current: none");
        println!("{channel} previous: none");
    }
}

fn print_status_entry(channel: Channel, name: &str, installation: Option<&Installation>) {
    match installation {
        Some(installation) => println!(
            "{channel} {name}: release={} version={}",
            installation.metadata.release, installation.metadata.version
        ),
        None => println!("{channel} {name}: none"),
    }
}

fn validate_nvim_exposure(paths: &Paths) -> Result<()> {
    let metadata = fs::symlink_metadata(&paths.nvim_link).map_err(|cause| {
        error(format!(
            "active channel exists but executable link {} is missing or inaccessible: {cause}",
            paths.nvim_link.display()
        ))
    })?;
    validate_managed_nvim_link(paths, &metadata)
}

fn validate_release_id(value: &str) -> Result<()> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) || value == "0" {
        return Err(error(format!(
            "invalid GitHub release ID '{value}': expected a positive decimal integer"
        )));
    }
    Ok(())
}

fn parse_api_digest(value: &str) -> Result<String> {
    let digest = value.strip_prefix("sha256:").ok_or_else(|| {
        error(format!(
            "invalid GitHub asset digest '{value}': expected sha256:<64 lowercase hexadecimal characters>"
        ))
    })?;
    validate_sha256(digest)?;
    Ok(digest.to_owned())
}

fn validate_sha256(value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(error(format!(
            "invalid SHA-256 digest '{value}': expected exactly 64 lowercase hexadecimal characters"
        )));
    }
    Ok(())
}

fn validate_download_url(value: &str) -> Result<()> {
    const PREFIX: &str = "https://github.com/neovim/neovim/releases/download/";
    if !value.starts_with(PREFIX)
        || value.bytes().any(|byte| byte.is_ascii_whitespace())
        || !value.ends_with(ASSET_NAME)
    {
        return Err(error(format!(
            "invalid Neovim release asset URL '{value}': expected an HTTPS github.com Neovim release URL ending in {ASSET_NAME}"
        )));
    }
    Ok(())
}

fn validate_version(value: &str, path: &Path) -> Result<()> {
    if !value.starts_with("NVIM v")
        || value.len() <= "NVIM v".len()
        || value.chars().any(char::is_control)
    {
        return Err(error(format!(
            "invalid Neovim version '{value}' at {}: expected the first --version line to start with 'NVIM v'",
            path.display()
        )));
    }
    Ok(())
}

fn command_failure(
    program: &str,
    operation: &str,
    output: &Output,
    path: Option<&Path>,
) -> NvError {
    let status = output.status.code().map_or_else(
        || "terminated by signal".to_owned(),
        |code| code.to_string(),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    let path = path.map_or_else(String::new, |path| format!(" for {}", path.display()));
    let details = if stderr.is_empty() {
        String::new()
    } else {
        format!(": {stderr}")
    };
    error(format!(
        "external program '{program}' failed during {operation}{path} with status {status}{details}"
    ))
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|cause| {
            error(format!(
                "failed to sync directory {}: {cause}",
                path.display()
            ))
        })
}

fn remove_staging_directory(path: &Path) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        error(format!(
            "refusing to clean staging path without parent: {}",
            path.display()
        ))
    })?;
    if parent.file_name() != Some(OsStr::new("staging")) {
        return Err(error(format!(
            "refusing to clean unmanaged staging path {}",
            path.display()
        )));
    }
    fs::remove_dir_all(path).map_err(|cause| {
        error(format!(
            "failed to remove staging directory {}: {cause}",
            path.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(character: char) -> String {
        std::iter::repeat_n(character, 64).collect()
    }

    fn metadata() -> InstallMetadata {
        InstallMetadata {
            channel: Channel::Nightly,
            release: "123456".to_owned(),
            version: "NVIM v0.13.0-dev-1234+gabcdef123".to_owned(),
            asset: ASSET_NAME.to_owned(),
            sha256: digest('a'),
        }
    }

    #[test]
    fn parses_exact_cli_grammar() {
        assert_eq!(
            Cli::parse([OsString::from("install"), OsString::from("stable")]).unwrap(),
            Cli::Install(Channel::Stable)
        );
        assert_eq!(
            Cli::parse([OsString::from("use"), OsString::from("nightly")]).unwrap(),
            Cli::Use(Channel::Nightly)
        );
        assert_eq!(
            Cli::parse([OsString::from("update")]).unwrap(),
            Cli::Update(ChannelSelection::All)
        );
        assert_eq!(
            Cli::parse([OsString::from("remove"), OsString::from("stable")]).unwrap(),
            Cli::Remove(ChannelSelection::Channel(Channel::Stable))
        );
        assert_eq!(
            Cli::parse([OsString::from("remove")]).unwrap(),
            Cli::Remove(ChannelSelection::All)
        );
        assert_eq!(
            Cli::parse([OsString::from("rollback"), OsString::from("stable")]).unwrap(),
            Cli::Rollback(Channel::Stable)
        );
        assert_eq!(Cli::parse([OsString::from("status")]).unwrap(), Cli::Status);
    }

    #[test]
    fn rejects_unsupported_cli_forms() {
        for arguments in [
            vec![],
            vec!["install"],
            vec!["install", "all"],
            vec!["use", "stable", "extra"],
            vec!["update", "all"],
            vec!["update", "all", "extra"],
            vec!["remove", "all"],
            vec!["remove", "nightly", "extra"],
            vec!["rollback"],
            vec!["rollback", "all"],
            vec!["status", "extra"],
            vec!["list"],
        ] {
            assert!(
                Cli::parse(arguments.into_iter().map(OsString::from)).is_err(),
                "accepted invalid arguments"
            );
        }
    }

    #[test]
    fn strictly_parses_api_digest() {
        let valid = digest('a');
        assert_eq!(parse_api_digest(&format!("sha256:{valid}")).unwrap(), valid);
        for invalid in [
            format!("sha512:{}", digest('a')),
            format!("sha256:{}", "a".repeat(63)),
            format!("sha256:{}", "A".repeat(64)),
            format!("sha256:{}", "g".repeat(64)),
        ] {
            assert!(parse_api_digest(&invalid).is_err());
        }
    }

    #[test]
    fn metadata_round_trips() {
        let expected = metadata();
        assert_eq!(
            InstallMetadata::parse(&expected.serialize(), Path::new("metadata")).unwrap(),
            expected
        );
    }

    #[test]
    fn metadata_rejects_schema_errors() {
        let valid = metadata().serialize();
        let cases = [
            valid.replace("channel=nightly\n", ""),
            valid.replace("channel=nightly\n", "channel=nightly\nchannel=nightly\n"),
            valid.replace("channel=nightly\n", "unknown=value\n"),
            valid.replace("asset=nvim-linux-x86_64.tar.gz", "asset=other.tar.gz"),
            valid.replace(&digest('a'), &digest('A')),
        ];
        for contents in cases {
            assert!(InstallMetadata::parse(&contents, Path::new("metadata")).is_err());
        }
    }

    #[test]
    fn validates_managed_install_names() {
        assert_eq!(
            parse_install_name("stable-123").unwrap(),
            (Channel::Stable, "123")
        );
        assert_eq!(
            parse_install_name("nightly-456").unwrap(),
            (Channel::Nightly, "456")
        );
        for invalid in ["stable", "nightly-abc", "other-123", "stable-../123"] {
            assert!(parse_install_name(invalid).is_err());
        }
    }

    #[test]
    fn validates_exact_channel_targets() {
        let link = Path::new("/tmp/current");
        assert_eq!(
            validate_channel_target(
                Path::new("../../installs/stable-123"),
                Channel::Stable,
                link
            )
            .unwrap(),
            "stable-123"
        );
        for target in [
            "../../../outside/stable-123",
            "/tmp/stable-123",
            "../../installs/nightly-123",
            "../../installs/stable-abc",
        ] {
            assert!(validate_channel_target(Path::new(target), Channel::Stable, link).is_err());
        }
    }

    #[test]
    fn home_must_be_absolute() {
        assert!(Paths::from_home(PathBuf::from("relative")).is_err());
        let paths = Paths::from_home(PathBuf::from("/home/tester")).unwrap();
        assert_eq!(paths.state, Path::new("/home/tester/.local/share/nv"));
        assert_eq!(paths.nvim_link, Path::new("/home/tester/.local/bin/nvim"));
    }
}
