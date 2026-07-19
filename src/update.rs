use std::{
    env, fmt, fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};

use anyhow::{anyhow, bail, Context, Result};

use crate::control::ControlRequests;

#[cfg(not(target_os = "windows"))]
use crate::platform;

const LATEST_RELEASE_API: &str = "https://api.github.com/repos/SakuraNeneCpp/CL4SE/releases/latest";
const RELEASE_DOWNLOAD_BASE: &str = "https://github.com/SakuraNeneCpp/CL4SE/releases/download";
const CHECKSUMS_ASSET: &str = "SHA256SUMS";

#[cfg(target_os = "windows")]
const WINDOWS_REPLACE_SCRIPT: &str = r#"param(
    [Parameter(Mandatory=$true)][int]$ParentProcessId,
    [Parameter(Mandatory=$true)][string]$Staged,
    [Parameter(Mandatory=$true)][string]$Target,
    [Parameter(Mandatory=$true)][string]$Backup,
    [Parameter(Mandatory=$true)][int]$Restart
)
$ErrorActionPreference = 'Stop'
try {
    Wait-Process -Id $ParentProcessId -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $Backup) {
        Remove-Item -LiteralPath $Backup -Force
    }
    Move-Item -LiteralPath $Target -Destination $Backup -Force
    try {
        Move-Item -LiteralPath $Staged -Destination $Target -Force
    } catch {
        Move-Item -LiteralPath $Backup -Destination $Target -Force
        throw
    }
    Remove-Item -LiteralPath $Backup -Force
    if ($Restart -eq 1) {
        & $Target start
        if ($LASTEXITCODE -ne 0) {
            throw "updated CL4SE failed to restart (exit $LASTEXITCODE)"
        }
    }
} catch {
    $_ | Out-String | Set-Content -LiteralPath "$Target.update-error.log" -Encoding UTF8
} finally {
    Remove-Item -LiteralPath $PSCommandPath -Force -ErrorAction SilentlyContinue
}
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl Version {
    fn parse(value: &str) -> Option<Self> {
        let mut parts = value.split('.');
        let version = Self {
            major: parts.next()?.parse().ok()?,
            minor: parts.next()?.parse().ok()?,
            patch: parts.next()?.parse().ok()?,
        };
        parts.next().is_none().then_some(version)
    }
}

struct PendingFile {
    path: PathBuf,
    armed: bool,
}

impl PendingFile {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PendingFile {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub fn update() -> Result<()> {
    let current_executable =
        env::current_exe().context("failed to locate the current CL4SE executable")?;
    let parent = current_executable
        .parent()
        .context("current CL4SE executable has no parent directory")?;
    let asset = current_asset_name()?;
    let process_id = std::process::id();
    let staged_path = parent.join(staged_file_name(process_id));
    let checksums_path = parent.join(format!(".cl4se-update-{process_id}-SHA256SUMS"));
    let release_path = parent.join(format!(".cl4se-update-{process_id}-release.json"));
    let mut staged = PendingFile::new(staged_path);
    let checksums = PendingFile::new(checksums_path);
    let release = PendingFile::new(release_path);

    download_to(LATEST_RELEASE_API, &release.path, "latest release metadata")?;
    let release_text = fs::read_to_string(&release.path).with_context(|| {
        format!(
            "failed to read latest release metadata: {}",
            release.path.display()
        )
    })?;
    let latest_version = release_version(&release_text)
        .context("latest GitHub release has no semantic vX.Y.Z tag")?;
    let current_version = Version::parse(env!("CARGO_PKG_VERSION"))
        .context("the running CL4SE version is not semantic x.y.z")?;
    match latest_version.cmp(&current_version) {
        std::cmp::Ordering::Less => bail!(
            "latest release v{latest_version} is older than running v{current_version}; refusing to downgrade"
        ),
        std::cmp::Ordering::Equal => {
            println!("CL4SE is already up to date (v{current_version}).");
            return Ok(());
        }
        std::cmp::Ordering::Greater => {}
    }

    download_to(
        &release_download_url(latest_version, CHECKSUMS_ASSET),
        &checksums.path,
        "release checksums",
    )?;
    download_to(
        &release_download_url(latest_version, asset),
        &staged.path,
        "latest CL4SE binary",
    )?;
    make_executable_like(&staged.path, &current_executable)?;

    let checksum_text = fs::read_to_string(&checksums.path).with_context(|| {
        format!(
            "failed to read downloaded checksums: {}",
            checksums.path.display()
        )
    })?;
    let expected_checksum = checksum_for_asset(&checksum_text, asset)
        .with_context(|| format!("{CHECKSUMS_ASSET} does not contain {asset}"))?;
    let actual_checksum = sha256(&staged.path)?;
    if !expected_checksum.eq_ignore_ascii_case(&actual_checksum) {
        bail!(
            "downloaded {asset} failed SHA-256 verification (expected {expected_checksum}, got {actual_checksum})"
        );
    }

    let control = ControlRequests::new()?;
    let was_running = control.probe_running(Duration::from_millis(750))?;
    if was_running && !control.stop_running(Duration::from_millis(750), Duration::from_secs(15))? {
        bail!("running CL4SE instance did not complete cleanup; update cancelled");
    }

    #[cfg(target_os = "windows")]
    {
        schedule_windows_replacement(
            &current_executable,
            &mut staged,
            current_version,
            latest_version,
            was_running,
        )?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        replace_unix_executable(&current_executable, &mut staged)?;
        if was_running {
            restart_background(&control)?;
        }
        println!("Updated CL4SE from v{current_version} to v{latest_version}.");
    }

    Ok(())
}

fn current_asset_name() -> Result<&'static str> {
    asset_name_for(env::consts::OS, env::consts::ARCH).ok_or_else(|| {
        anyhow!(
            "self-update is unavailable for {} {}; download the matching release asset manually",
            env::consts::OS,
            env::consts::ARCH
        )
    })
}

fn asset_name_for(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("windows", "x86_64") => Some("cl4se-windows-x86_64.exe"),
        ("macos", "x86_64" | "aarch64") => Some("cl4se-macos-universal"),
        ("linux", "x86_64") => Some("cl4se-linux-x86_64"),
        _ => None,
    }
}

fn staged_file_name(process_id: u32) -> String {
    if cfg!(target_os = "windows") {
        format!(".cl4se-stage-{process_id}.exe")
    } else {
        format!(".cl4se-stage-{process_id}")
    }
}

fn release_download_url(version: Version, asset: &str) -> String {
    format!("{RELEASE_DOWNLOAD_BASE}/v{version}/{asset}")
}

fn release_version(contents: &str) -> Option<Version> {
    let mut remaining = contents;
    while let Some(index) = remaining.find("\"tag_name\"") {
        remaining = &remaining[index + "\"tag_name\"".len()..];
        let value = remaining
            .trim_start()
            .strip_prefix(':')?
            .trim_start()
            .strip_prefix('"')?;
        let end = value.find('"')?;
        let tag = &value[..end];
        if !tag
            .bytes()
            .any(|byte| byte == b'\\' || byte.is_ascii_control())
        {
            return tag.strip_prefix('v').and_then(Version::parse);
        }
        remaining = &value[end + 1..];
    }
    None
}

fn checksum_for_asset<'a>(contents: &'a str, asset: &str) -> Option<&'a str> {
    contents.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let checksum = fields.next()?;
        let name = fields.next()?.trim_start_matches('*');
        (name == asset
            && checksum.len() == 64
            && checksum.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some(checksum)
    })
}

fn command_output(mut command: Command, description: &str) -> Result<Output> {
    let output = command
        .output()
        .with_context(|| format!("failed to start {description}"))?;
    if !output.status.success() {
        bail!(
            "{description} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output)
}

#[cfg(target_os = "windows")]
fn download_to(url: &str, destination: &Path, description: &str) -> Result<()> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut command = Command::new("powershell.exe");
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$ProgressPreference='SilentlyContinue'; Invoke-WebRequest -UseBasicParsing -Uri $env:CL4SE_UPDATE_URL -OutFile $env:CL4SE_UPDATE_DESTINATION",
        ])
        .env("CL4SE_UPDATE_URL", url)
        .env("CL4SE_UPDATE_DESTINATION", destination)
        .creation_flags(CREATE_NO_WINDOW);
    command_output(command, description).map(|_| ())
}

#[cfg(not(target_os = "windows"))]
fn download_to(url: &str, destination: &Path, description: &str) -> Result<()> {
    let mut command = Command::new("curl");
    command
        .args(["-fL", "--retry", "2", "--output"])
        .arg(destination)
        .arg(url);
    command_output(command, description).map(|_| ())
}

#[cfg(target_os = "windows")]
fn sha256(path: &Path) -> Result<String> {
    let mut command = Command::new("certutil.exe");
    command.args(["-hashfile"]).arg(path).arg("SHA256");
    let output = command_output(command, "SHA-256 verification")?;
    let stdout = String::from_utf8(output.stdout).context("SHA-256 output is not UTF-8")?;
    checksum_from_localized_output(&stdout).context("certutil returned no SHA-256 checksum")
}

#[cfg(target_os = "macos")]
fn sha256(path: &Path) -> Result<String> {
    let mut command = Command::new("shasum");
    command.args(["-a", "256"]).arg(path);
    checksum_from_command(command, "shasum")
}

#[cfg(target_os = "linux")]
fn sha256(path: &Path) -> Result<String> {
    let mut command = Command::new("sha256sum");
    command.arg(path);
    checksum_from_command(command, "sha256sum")
}

#[cfg(not(target_os = "windows"))]
fn checksum_from_command(command: Command, description: &str) -> Result<String> {
    let output = command_output(command, description)?;
    let stdout = String::from_utf8(output.stdout).context("SHA-256 output is not UTF-8")?;
    stdout
        .split_whitespace()
        .next()
        .map(str::to_owned)
        .context("SHA-256 command returned no checksum")
}

#[cfg(any(target_os = "windows", test))]
fn checksum_from_localized_output(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let candidate = line
            .bytes()
            .filter(|byte| !byte.is_ascii_whitespace())
            .collect::<Vec<_>>();
        (candidate.len() == 64 && candidate.iter().all(u8::is_ascii_hexdigit))
            .then(|| String::from_utf8(candidate).ok())
            .flatten()
    })
}

#[cfg(unix)]
fn make_executable_like(downloaded: &Path, current: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = fs::metadata(current)
        .with_context(|| format!("failed to inspect permissions: {}", current.display()))?
        .permissions()
        .mode();
    fs::set_permissions(downloaded, fs::Permissions::from_mode(mode | 0o100)).with_context(|| {
        format!(
            "failed to make downloaded CL4SE executable: {}",
            downloaded.display()
        )
    })
}

#[cfg(windows)]
fn make_executable_like(_downloaded: &Path, _current: &Path) -> Result<()> {
    Ok(())
}

#[cfg(target_os = "windows")]
fn schedule_windows_replacement(
    current: &Path,
    staged: &mut PendingFile,
    current_version: Version,
    latest_version: Version,
    restart: bool,
) -> Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let process_id = std::process::id();
    let script_path = current.with_file_name(format!(".cl4se-update-{process_id}.ps1"));
    let backup_path = current.with_file_name(format!(".cl4se-update-{process_id}.backup.exe"));
    fs::write(&script_path, WINDOWS_REPLACE_SCRIPT)
        .with_context(|| format!("failed to write update helper: {}", script_path.display()))?;
    let mut script = PendingFile::new(script_path);

    let mut command = Command::new("powershell.exe");
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script.path)
        .arg("-ParentProcessId")
        .arg(process_id.to_string())
        .arg("-Staged")
        .arg(&staged.path)
        .arg("-Target")
        .arg(current)
        .arg("-Backup")
        .arg(&backup_path)
        .arg("-Restart")
        .arg(if restart { "1" } else { "0" })
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .context("failed to start the Windows update helper")?;

    staged.disarm();
    script.disarm();
    println!(
        "Verified CL4SE v{latest_version}. The update from v{current_version} will complete after this command exits."
    );
    if restart {
        println!("The background instance will restart automatically.");
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn replace_unix_executable(current: &Path, staged: &mut PendingFile) -> Result<()> {
    let backup = current.with_file_name(format!(".cl4se-update-{}.backup", std::process::id()));
    if backup.exists() {
        fs::remove_file(&backup).with_context(|| {
            format!("failed to remove stale update backup: {}", backup.display())
        })?;
    }
    fs::rename(current, &backup)
        .with_context(|| format!("failed to create update backup: {}", backup.display()))?;
    if let Err(error) = fs::rename(&staged.path, current) {
        let rollback = fs::rename(&backup, current);
        return match rollback {
            Ok(()) => Err(error).context("failed to install downloaded CL4SE; restored old binary"),
            Err(rollback_error) => Err(anyhow!(
                "failed to install downloaded CL4SE ({error}) and rollback failed ({rollback_error}); backup remains at {}",
                backup.display()
            )),
        };
    }
    staged.disarm();
    fs::remove_file(&backup)
        .with_context(|| format!("failed to remove update backup: {}", backup.display()))?;
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn restart_background(control: &ControlRequests) -> Result<()> {
    let process = platform::start_background()?;
    if !control.probe_running(Duration::from_secs(5))? {
        bail!(
            "updated CL4SE did not restart; inspect {}",
            process.log_path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_assets_match_supported_targets() {
        assert_eq!(
            asset_name_for("windows", "x86_64"),
            Some("cl4se-windows-x86_64.exe")
        );
        assert_eq!(
            asset_name_for("macos", "aarch64"),
            Some("cl4se-macos-universal")
        );
        assert_eq!(
            asset_name_for("macos", "x86_64"),
            Some("cl4se-macos-universal")
        );
        assert_eq!(
            asset_name_for("linux", "x86_64"),
            Some("cl4se-linux-x86_64")
        );
        assert_eq!(asset_name_for("linux", "aarch64"), None);
    }

    #[test]
    fn release_asset_url_is_pinned_to_the_discovered_version() {
        assert_eq!(
            release_download_url(
                Version {
                    major: 1,
                    minor: 0,
                    patch: 3
                },
                "cl4se-linux-x86_64"
            ),
            "https://github.com/SakuraNeneCpp/CL4SE/releases/download/v1.0.3/cl4se-linux-x86_64"
        );
    }

    #[test]
    fn latest_release_parser_requires_an_unescaped_semantic_tag() {
        assert_eq!(
            release_version(r#"{"name":"CL4SE","tag_name" : "v1.0.3"}"#),
            Some(Version {
                major: 1,
                minor: 0,
                patch: 3
            })
        );
        assert_eq!(release_version(r#"{"tag_name":"1.0.3"}"#), None);
        assert_eq!(release_version(r#"{"tag_name":"v1.0.3\\u0000"}"#), None);
    }

    #[test]
    fn checksum_parser_requires_an_exact_asset_name() {
        let contents = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  cl4se-linux-x86_64-old\nBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB *cl4se-linux-x86_64\n";
        assert_eq!(
            checksum_for_asset(contents, "cl4se-linux-x86_64"),
            Some("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB")
        );
        assert_eq!(
            checksum_for_asset(contents, "cl4se-windows-x86_64.exe"),
            None
        );
    }

    #[test]
    fn localized_hash_output_parser_selects_only_a_full_sha256() {
        let output = "SHA256 hash of file:\n11 22 33 44 55 66 77 88 99 aa bb cc dd ee ff 00 11 22 33 44 55 66 77 88 99 aa bb cc dd ee ff 00\nCertUtil: command completed successfully.\n";
        assert_eq!(
            checksum_from_localized_output(output),
            Some("112233445566778899aabbccddeeff00112233445566778899aabbccddeeff00".to_owned())
        );
        assert_eq!(checksum_from_localized_output("not a hash"), None);
    }

    #[test]
    fn semantic_versions_are_ordered_numerically() {
        let current = Version::parse("1.0.9").expect("valid version");
        let latest = Version::parse("1.0.10").expect("valid version");
        assert!(latest > current);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_helper_waits_rolls_back_and_restarts() {
        assert!(WINDOWS_REPLACE_SCRIPT.contains("Wait-Process"));
        assert!(
            WINDOWS_REPLACE_SCRIPT.contains("Move-Item -LiteralPath $Backup -Destination $Target")
        );
        assert!(WINDOWS_REPLACE_SCRIPT.contains("& $Target start"));
        assert!(WINDOWS_REPLACE_SCRIPT.contains("$Target.update-error.log"));
    }
}
