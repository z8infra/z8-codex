//! Explicit installation of the native Codex Desktop package from the Z8 mirror.
//!
//! The caller is responsible for showing a consent dialog and calling this only
//! after the user chooses to install. This module never falls back to an
//! arbitrary URL or an unverified installer.

use anyhow::{bail, ensure, Context, Result};
use async_trait::async_trait;
use futures_util::{stream, StreamExt, TryStreamExt};
use quick_xml::events::Event;
use reqwest::{redirect::Policy, Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU8, Ordering},
    time::Duration,
};
use tokio::sync::Notify;

pub const CODEX_DESKTOP_MIRROR_MANIFEST_URL: &str =
    "https://download.z8.hk/z8-launch/codex-mirror/latest.json";
const PUBLIC_ORIGIN: &str = "https://download.z8.hk/";
const MIRROR_PATH: &str = "z8-launch/codex-mirror/";
const WINDOWS_X64_ASSET: &str = "Codex-windows-x64.msix";
#[cfg(test)]
const ASSET_FILENAME: &str = WINDOWS_X64_ASSET;
const PACKAGE_NAME: &[u8] = b"OpenAI.Codex";
const PACKAGE_PUBLISHER: &[u8] = b"CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B";
const MAX_MANIFEST_BYTES: usize = 2 * 1024 * 1024;
const MAX_MSIX_MANIFEST_BYTES: u64 = 2 * 1024 * 1024;
const MAX_PART_BYTES: u64 = 50 * 1024 * 1024;
const MAX_ARTIFACT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
// Keep a bounded manifest while leaving room for desktop package growth.
// MAX_ARTIFACT_BYTES remains the aggregate size limit.
const MAX_PARTS: usize = 256;
const DOWNLOAD_CONCURRENCY: usize = 4;
// A single part can take several minutes on a constrained connection when
// four parts share the available bandwidth.  The previous request timeout
// was shorter than that, so otherwise healthy downloads were aborted near the
// end of a part.  Keep an idle read limit while allowing a slow but moving
// transfer to finish, then retry transient failures at the part boundary.
const DOWNLOAD_REQUEST_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const DOWNLOAD_READ_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_DOWNLOAD_ATTEMPTS: usize = 3;
const DOWNLOAD_RETRY_BASE_DELAY: Duration = Duration::from_secs(1);
#[cfg(windows)]
const ELEVATED_COMMAND_MARKER: &str = "__Z8_CODEX_DESKTOP_ELEVATED_COMMAND__";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DesktopMirrorTarget {
    WindowsX64,
    WindowsArm64,
    MacosX64,
    MacosArm64,
}

impl DesktopMirrorTarget {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WindowsX64 => "windows-x64",
            Self::WindowsArm64 => "windows-arm64",
            Self::MacosX64 => "macos-x64",
            Self::MacosArm64 => "macos-arm64",
        }
    }

    pub const fn asset_filename(self) -> &'static str {
        match self {
            Self::WindowsX64 => WINDOWS_X64_ASSET,
            Self::WindowsArm64 => "Codex-windows-arm64.msix",
            Self::MacosX64 => "Codex-macos-x64.dmg",
            Self::MacosArm64 => "Codex-macos-arm64.dmg",
        }
    }

    fn upstream_url(self) -> &'static str {
        match self {
            Self::WindowsX64 | Self::WindowsArm64 => {
                "https://get.microsoft.com/installer/download/9PLM9XGG6VKS?cid=website_cta_psi"
            }
            Self::MacosX64 => {
                "https://persistent.oaistatic.com/codex-app-prod/Codex-latest-x64.dmg"
            }
            Self::MacosArm64 => "https://persistent.oaistatic.com/codex-app-prod/Codex.dmg",
        }
    }

    fn is_windows(self) -> bool {
        matches!(self, Self::WindowsX64 | Self::WindowsArm64)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopMirrorEnvironment {
    pub platform: &'static str,
    pub target: Option<DesktopMirrorTarget>,
    pub reason: Option<DesktopMirrorUnsupportedReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopMirrorUnsupportedReason {
    UnsupportedPlatform,
    OsVersionTooOld,
    ArchitectureUnknown,
}

/// Stable error used by the Manager to show a platform-specific unpublished
/// mirror message without parsing a human-readable error string.
#[derive(Debug, thiserror::Error)]
#[error("Codex Desktop mirror asset is not published for {target}")]
pub struct MissingMirrorAsset {
    pub target: &'static str,
}

#[derive(Debug, thiserror::Error)]
#[error("Codex Desktop installation cancelled")]
pub struct MirrorInstallCancelled;

/// The Windows package command ran, but AppX rejected the package. Keep this
/// typed so the manager can show a useful recovery message without exposing
/// PowerShell output or local paths in the WebView.
#[derive(Debug, thiserror::Error)]
#[error("Windows rejected Codex Desktop installation")]
pub struct WindowsPackageInstallError {
    pub exit_code: Option<i32>,
    pub hresult: Option<String>,
}

impl WindowsPackageInstallError {
    pub fn requires_admin(&self) -> bool {
        self.hresult
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case("0X80073D28"))
    }

    pub fn cancelled(&self) -> bool {
        self.exit_code == Some(1223)
            || self
                .hresult
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case("0X800704C7"))
    }
}

fn download_cache_root() -> PathBuf {
    directories::BaseDirs::new()
        .map(|dirs| dirs.cache_dir().join("z8-codex-desktop"))
        .unwrap_or_else(|| std::env::temp_dir().join("z8-codex-desktop"))
}

/// Cancellation is accepted only before the operating system starts installing.
#[derive(Default)]
pub struct MirrorInstallControl {
    state: AtomicU8,
    wake: Notify,
}

impl MirrorInstallControl {
    pub fn cancel(&self) -> bool {
        match self.state.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => {
                self.wake.notify_one();
                true
            }
            Err(state) => state == 1,
        }
    }

    fn check(&self) -> Result<()> {
        if self.state.load(Ordering::Acquire) == 1 {
            Err(MirrorInstallCancelled.into())
        } else {
            Ok(())
        }
    }

    async fn cancelled(&self) {
        while self.state.load(Ordering::Acquire) != 1 {
            self.wake.notified().await;
        }
    }

    async fn run<T>(&self, future: impl std::future::Future<Output = Result<T>>) -> Result<T> {
        tokio::select! {
            biased;
            _ = self.cancelled() => Err(MirrorInstallCancelled.into()),
            result = future => result,
        }
    }

    fn begin_installation(&self) -> Result<()> {
        self.state.compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| MirrorInstallCancelled.into())
    }
}

pub fn has_resumable_download(target: DesktopMirrorTarget) -> bool {
    has_cached_download(&download_cache_root(), target)
}

fn has_cached_download(root: &Path, target: DesktopMirrorTarget) -> bool {
    let Ok(entries) = fs::read_dir(root.join(target.as_str())) else { return false };
    entries.filter_map(Result::ok).any(|entry| {
        let name = entry.file_name();
        let Some(hash) = name.to_str() else { return false };
        validate_sha256(hash).is_ok()
            && entry.file_type().is_ok_and(|kind| kind.is_dir())
            && [target.asset_filename().to_owned(), format!("{}.partial", target.asset_filename())]
                .iter().any(|name| fs::symlink_metadata(entry.path().join(name))
                    .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0 && metadata.len() <= MAX_ARTIFACT_BYTES))
    })
}

fn select_environment(
    platform: &'static str,
    architecture: Option<&str>,
    os_supported: bool,
) -> DesktopMirrorEnvironment {
    let target = if os_supported {
        match (platform, architecture) {
            ("windows", Some("x86_64")) => Some(DesktopMirrorTarget::WindowsX64),
            ("windows", Some("aarch64")) => Some(DesktopMirrorTarget::WindowsArm64),
            ("macos", Some("x86_64")) => Some(DesktopMirrorTarget::MacosX64),
            ("macos", Some("aarch64")) => Some(DesktopMirrorTarget::MacosArm64),
            _ => None,
        }
    } else {
        None
    };
    let reason = if !matches!(platform, "windows" | "macos") {
        Some(DesktopMirrorUnsupportedReason::UnsupportedPlatform)
    } else if !os_supported {
        Some(DesktopMirrorUnsupportedReason::OsVersionTooOld)
    } else if target.is_none() {
        Some(DesktopMirrorUnsupportedReason::ArchitectureUnknown)
    } else {
        None
    };
    DesktopMirrorEnvironment {
        platform,
        target,
        reason,
    }
}

/// Conservative precheck only. The package's declared minimum OS and the
/// platform installer remain the final compatibility gates.
pub fn inspect_desktop_mirror_environment() -> DesktopMirrorEnvironment {
    #[cfg(windows)]
    {
        use windows::Win32::System::{
            SystemInformation::{
                IMAGE_FILE_MACHINE, IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_ARM64,
            },
            Threading::{GetCurrentProcess, IsWow64Process2},
        };
        let mut process = IMAGE_FILE_MACHINE::default();
        let mut native = IMAGE_FILE_MACHINE::default();
        let arch = unsafe { IsWow64Process2(GetCurrentProcess(), &mut process, Some(&mut native)) }
            .ok()
            .and_then(|()| match native {
                IMAGE_FILE_MACHINE_AMD64 => Some("x86_64"),
                IMAGE_FILE_MACHINE_ARM64 => Some("aarch64"),
                _ => None,
            });
        let version = windows_version::OsVersion::current();
        return select_environment(
            "windows",
            arch,
            version.major > 10 || (version.major == 10 && version.build >= 17763),
        );
    }
    #[cfg(target_os = "macos")]
    {
        let arch = macos_native_architecture();
        let version = std::process::Command::new("/usr/bin/sw_vers")
            .arg("-productVersion")
            .output()
            .ok()
            .and_then(|output| output.status.success().then_some(output.stdout))
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .and_then(|value| value.trim().split('.').next()?.parse::<u32>().ok());
        return select_environment("macos", arch, version.is_some_and(|major| major >= 14));
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    select_environment("unsupported", None, false)
}

#[cfg(target_os = "macos")]
fn macos_native_architecture() -> Option<&'static str> {
    // The first probe detects an x64 process under Rosetta. The second checks
    // physical Apple Silicon even for a native arm64 binary. Intel Macs may
    // not expose hw.optional.arm64, so uname is used only after Rosetta is
    // explicitly ruled out or its sysctl is unavailable on older Intel Macs.
    fn probe(name: &str) -> Option<String> {
        let output = std::process::Command::new("/usr/sbin/sysctl")
            .args(["-n", name])
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| {
                String::from_utf8(output.stdout)
                    .ok()
                    .map(|s| s.trim().to_owned())
            })
            .flatten()
    }
    if probe("sysctl.proc_translated").as_deref() == Some("1")
        || probe("hw.optional.arm64").as_deref() == Some("1")
    {
        return Some("aarch64");
    }
    let output = std::process::Command::new("/usr/bin/uname")
        .arg("-m")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    match String::from_utf8(output.stdout).ok()?.trim() {
        "arm64" => Some("aarch64"),
        "x86_64" if probe("sysctl.proc_translated").as_deref() != Some("1") => Some("x86_64"),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MirrorPhase {
    Checking,
    Downloading,
    Validating,
    Installing,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorProgress {
    pub phase: MirrorPhase,
    pub completed: u64,
    pub total: u64,
}

/// Download and install the pinned official Windows x64 package. The caller
/// must obtain an explicit user click before invoking this function. Progress
/// is expressed in verified/downloaded bytes, not a claimed installation ETA.
pub async fn install_windows_x64_from_mirror<F>(progress: F) -> Result<()>
where
    F: Fn(MirrorProgress) + Send + Sync,
{
    ensure!(
        inspect_desktop_mirror_environment().target == Some(DesktopMirrorTarget::WindowsX64),
        "Codex Desktop mirror installation requires native Windows x64"
    );
    install_for_target(DesktopMirrorTarget::WindowsX64, progress).await
}

pub async fn install_for_current_platform<F>(progress: F) -> Result<()>
where
    F: Fn(MirrorProgress) + Send + Sync,
{
    install_for_current_platform_with_cancel(progress, &MirrorInstallControl::default()).await
}

pub async fn install_for_current_platform_with_cancel<F>(
    progress: F,
    control: &MirrorInstallControl,
) -> Result<()>
where
    F: Fn(MirrorProgress) + Send + Sync,
{
    let environment = inspect_desktop_mirror_environment();
    let target = environment.target.ok_or_else(|| {
        anyhow::anyhow!(
            "Codex Desktop environment is unsupported: {:?}",
            environment.reason
        )
    })?;
    install_for_target_with_cancel(target, progress, control).await
}

async fn install_for_target<F>(target: DesktopMirrorTarget, progress: F) -> Result<()>
where
    F: Fn(MirrorProgress) + Send + Sync,
{
    install_for_target_with_cancel(target, progress, &MirrorInstallControl::default()).await
}

async fn install_for_target_with_cancel<F>(
    target: DesktopMirrorTarget,
    progress: F,
    control: &MirrorInstallControl,
) -> Result<()>
where
    F: Fn(MirrorProgress) + Send + Sync,
{
    let client = Client::builder()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .timeout(DOWNLOAD_REQUEST_TIMEOUT)
        .read_timeout(DOWNLOAD_READ_TIMEOUT)
        .user_agent(format!("Z8Codex/{}", env!("CARGO_PKG_VERSION")))
        .build()?;
    install_target_with_cancel(
        &client,
        &MirrorEndpoint::production(),
        target,
        &SystemPackageInstaller { target },
        progress,
        control,
        &download_cache_root(),
    )
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MirrorManifest {
    schema_version: u32,
    mirror_provider: String,
    upstream_repository: String,
    tag: String,
    version: String,
    upstream_release_url: String,
    mirror_repository: String,
    generated_at: String,
    assets: Vec<MirrorAsset>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MirrorAsset {
    filename: String,
    upstream_url: String,
    size_bytes: u64,
    sha256: String,
    parts: Vec<MirrorPart>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MirrorPart {
    filename: String,
    mirror_url: String,
    size_bytes: u64,
    sha256: String,
    encoding: String,
    decoded_size_bytes: u64,
}

struct MirrorEndpoint {
    origin: String,
}

impl MirrorEndpoint {
    fn production() -> Self {
        Self {
            origin: PUBLIC_ORIGIN.into(),
        }
    }

    fn manifest_url(&self) -> String {
        format!("{}{MIRROR_PATH}latest.json", self.origin)
    }

    fn part_url(&self, tag: &str, filename: &str) -> String {
        format!("{}{MIRROR_PATH}{tag}/{filename}", self.origin)
    }
}

#[async_trait]
trait PackageInstaller: Send + Sync {
    async fn install(&self, path: &Path) -> Result<()>;
}

struct SystemPackageInstaller {
    target: DesktopMirrorTarget,
}

#[async_trait]
impl PackageInstaller for SystemPackageInstaller {
    async fn install(&self, path: &Path) -> Result<()> {
        match self.target {
            DesktopMirrorTarget::WindowsX64 | DesktopMirrorTarget::WindowsArm64 => {
                install_verified_msix(path).await
            }
            DesktopMirrorTarget::MacosX64 | DesktopMirrorTarget::MacosArm64 => {
                install_verified_dmg(path, self.target).await
            }
        }
    }
}

#[cfg(test)]
async fn install_target_with<F, I>(
    client: &Client,
    endpoint: &MirrorEndpoint,
    target: DesktopMirrorTarget,
    installer: &I,
    progress: F,
) -> Result<()>
where
    F: Fn(MirrorProgress) + Send + Sync,
    I: PackageInstaller,
{
    let cache = tempfile::tempdir()?;
    install_target_with_cancel(
        client,
        endpoint,
        target,
        installer,
        progress,
        &MirrorInstallControl::default(),
        cache.path(),
    )
    .await
}

async fn install_target_with_cancel<F, I>(
    client: &Client,
    endpoint: &MirrorEndpoint,
    target: DesktopMirrorTarget,
    installer: &I,
    progress: F,
    control: &MirrorInstallControl,
    cache_root: &Path,
) -> Result<()>
where
    F: Fn(MirrorProgress) + Send + Sync,
    I: PackageInstaller,
{
    control.check()?;
    progress(MirrorProgress {
        phase: MirrorPhase::Checking,
        completed: 0,
        total: 0,
    });
    let manifest_url = endpoint.manifest_url();
    let manifest_bytes = control.run(fetch_manifest(client, &manifest_url)).await?;
    let manifest: MirrorManifest = serde_json::from_slice(&manifest_bytes)
        .context("Codex Desktop mirror manifest has an invalid schema")?;
    let asset = validate_manifest(&manifest, endpoint, target)?;

    let temp = DownloadPackage::open(cache_root, target, &asset.sha256)?;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&temp.partial)
        .context("cannot create temporary Codex Desktop download")?;
    let (start_part, mut completed) = prepare_partial(&mut file, &asset.parts, control)?;
    let last_reported = completed;
    progress(MirrorProgress {
        phase: MirrorPhase::Downloading,
        completed,
        total: asset.size_bytes,
    });
    let mut part_start = asset
        .parts
        .iter()
        .take(start_part)
        .map(|part| part.size_bytes)
        .sum::<u64>();
    let jobs: Vec<_> = asset
        .parts
        .iter()
        .skip(start_part)
        .map(|part| {
            let start = part_start;
            part_start += part.size_bytes;
            (start, part.clone())
        })
        .collect();
    let completed_bytes = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(completed));
    let last_reported_bytes = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(last_reported));
    let tag = manifest.tag.clone();
    let file_ref = &file;
    let progress_ref = &progress;
    let total_bytes = asset.size_bytes;
    let completed_for_stream = std::sync::Arc::clone(&completed_bytes);
    let last_reported_for_stream = std::sync::Arc::clone(&last_reported_bytes);
    stream::iter(jobs)
        .map(move |(part_start, part)| {
            let tag = tag.clone();
            let completed_bytes = std::sync::Arc::clone(&completed_for_stream);
            let last_reported_bytes = std::sync::Arc::clone(&last_reported_for_stream);
            async move {
                download_part(
                    client,
                    endpoint,
                    &tag,
                    &part,
                    part_start,
                    file_ref,
                    &completed_bytes,
                    &last_reported_bytes,
                    total_bytes,
                    progress_ref,
                    control,
                )
                .await
            }
        })
        .buffer_unordered(DOWNLOAD_CONCURRENCY)
        .try_collect::<Vec<_>>()
        .await?;
    completed = completed_bytes.load(std::sync::atomic::Ordering::Acquire);
    let last_reported = last_reported_bytes.load(std::sync::atomic::Ordering::Acquire);
    if completed != last_reported {
        progress(MirrorProgress {
            phase: MirrorPhase::Downloading,
            completed,
            total: asset.size_bytes,
        });
    }
    control.check()?;
    ensure!(
        completed == asset.size_bytes,
        "Codex Desktop artifact is incomplete"
    );
    file.sync_all()
        .context("cannot sync Codex Desktop download")?;
    progress(MirrorProgress {
        phase: MirrorPhase::Validating,
        completed,
        total: asset.size_bytes,
    });
    file.seek(SeekFrom::Start(0))?;
    let whole_digest = hash_prefix(&mut file, asset.size_bytes, control)?;
    ensure!(
        digest_matches(&whole_digest.finalize(), &asset.sha256),
        "Codex Desktop artifact digest mismatch"
    );
    drop(file);
    // Windows package identity is checked before Add-AppxPackage performs its
    // signature gate. macOS verifies the DMG and signed app after mounting.
    if target.is_windows() {
        validate_msix_identity(&temp.partial, target)?;
    }
    control.begin_installation()?;
    // A process crash can leave the finalized package beside the partial file.
    // Clear that stale artifact before renaming so a later retry is not
    // rejected solely because the destination already exists.
    if temp.package.exists() {
        fs::remove_file(&temp.package).context("cannot clear stale Codex Desktop package")?;
    }
    fs::rename(&temp.partial, &temp.package).context("cannot finalize Codex Desktop download")?;
    progress(MirrorProgress {
        phase: MirrorPhase::Installing,
        completed,
        total: asset.size_bytes,
    });
    if let Err(error) = installer.install(&temp.package).await {
        // Add-AppxPackage can fail after the download has been fully verified
        // (for example because Windows still has a stale package registration,
        // a file is locked, or deployment needs to be retried). Put the
        // finalized package back in the resumable slot so a retry performs
        // validation and installation without downloading hundreds of MB
        // again.
        preserve_failed_install(&temp);
        return Err(error);
    }
    temp.remove_completed();
    progress(MirrorProgress {
        phase: MirrorPhase::Complete,
        completed,
        total: asset.size_bytes,
    });
    Ok(())
}

async fn download_part<F>(
    client: &Client,
    endpoint: &MirrorEndpoint,
    tag: &str,
    part: &MirrorPart,
    part_start: u64,
    file: &File,
    completed_bytes: &std::sync::Arc<std::sync::atomic::AtomicU64>,
    last_reported_bytes: &std::sync::Arc<std::sync::atomic::AtomicU64>,
    total_bytes: u64,
    progress: &F,
    control: &MirrorInstallControl,
) -> Result<()>
where
    F: Fn(MirrorProgress) + Send + Sync,
{
    let mut last_error = None;
    for attempt in 1..=MAX_DOWNLOAD_ATTEMPTS {
        match download_part_once(
            client,
            endpoint,
            tag,
            part,
            part_start,
            file,
            completed_bytes,
            last_reported_bytes,
            total_bytes,
            progress,
            control,
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(error) if error.downcast_ref::<MirrorInstallCancelled>().is_some() => {
                return Err(error)
            }
            Err(error) => {
                last_error = Some(error);
                if attempt == MAX_DOWNLOAD_ATTEMPTS {
                    break;
                }
                let delay = DOWNLOAD_RETRY_BASE_DELAY
                    .checked_mul(attempt as u32)
                    .unwrap_or(DOWNLOAD_RETRY_BASE_DELAY);
                tokio::select! {
                    biased;
                    _ = control.cancelled() => return Err(MirrorInstallCancelled.into()),
                    _ = tokio::time::sleep(delay) => {}
                }
            }
        }
    }
    // Preserve the original error so diagnostics and tests can still identify
    // whether the final attempt failed on transport, size, or digest checks.
    Err(last_error.expect("a failed download attempt must provide an error"))
}

async fn download_part_once<F>(
    client: &Client,
    endpoint: &MirrorEndpoint,
    tag: &str,
    part: &MirrorPart,
    part_start: u64,
    file: &File,
    completed_bytes: &std::sync::Arc<std::sync::atomic::AtomicU64>,
    last_reported_bytes: &std::sync::Arc<std::sync::atomic::AtomicU64>,
    total_bytes: u64,
    progress: &F,
    control: &MirrorInstallControl,
) -> Result<()>
where
    F: Fn(MirrorProgress) + Send + Sync,
{
    let mut received = 0_u64;
    let result = async {
        control.check()?;
        let expected_url = endpoint.part_url(tag, &part.filename);
        let mut response = control
            .run(get_part(client, &expected_url, 0, part.size_bytes))
            .await?;
        if let Some(length) = response.content_length() {
            ensure!(length == part.size_bytes, "Codex Desktop part size mismatch");
        }
        let mut digest = Sha256::new();
        loop {
            let chunk = tokio::select! {
                biased;
                _ = control.cancelled() => return Err(MirrorInstallCancelled.into()),
                chunk = response.chunk() => chunk.context("Codex Desktop part download failed")?,
            };
            let Some(chunk) = chunk else { break };
            control.check()?;
            let next_received = received
                .checked_add(chunk.len() as u64)
                .context("Codex Desktop part size overflow")?;
            ensure!(
                next_received <= part.size_bytes,
                "Codex Desktop part exceeds declared size"
            );
            write_at_all(file, &chunk, part_start + received)?;
            digest.update(&chunk);
            received = next_received;
            let completed = completed_bytes
                .fetch_add(chunk.len() as u64, std::sync::atomic::Ordering::AcqRel)
                .checked_add(chunk.len() as u64)
                .context("Codex Desktop artifact size overflow")?;
            ensure!(
                completed <= total_bytes,
                "Codex Desktop artifact exceeds declared size"
            );
            let reported = last_reported_bytes.load(std::sync::atomic::Ordering::Acquire);
            if completed.saturating_sub(reported) >= 1024 * 1024 || completed == total_bytes {
                if last_reported_bytes
                    .compare_exchange(
                        reported,
                        completed,
                        std::sync::atomic::Ordering::AcqRel,
                        std::sync::atomic::Ordering::Acquire,
                    )
                    .is_ok()
                {
                    progress(MirrorProgress {
                        phase: MirrorPhase::Downloading,
                        completed,
                        total: total_bytes,
                    });
                }
            }
        }
        ensure!(received == part.size_bytes, "Codex Desktop part is incomplete");
        ensure!(
            digest_matches(&digest.finalize(), &part.sha256),
            "Codex Desktop part digest mismatch"
        );
        Ok(())
    }
    .await;

    // A failed attempt may have written only part of this part.  Remove those
    // bytes from the aggregate progress before the retry so progress never
    // exceeds the actual artifact size.
    if result.is_err() && received > 0 {
        let previous = completed_bytes.fetch_sub(received, std::sync::atomic::Ordering::AcqRel);
        let current = previous.saturating_sub(received);
        let mut reported = last_reported_bytes.load(std::sync::atomic::Ordering::Acquire);
        while reported > current {
            match last_reported_bytes.compare_exchange(
                reported,
                current,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(actual) => reported = actual,
            }
        }
    }
    result
}

fn write_at_all(file: &File, mut bytes: &[u8], mut offset: u64) -> Result<()> {
    while !bytes.is_empty() {
        let written = write_at(file, bytes, offset)
            .context("cannot write Codex Desktop download")?;
        ensure!(written > 0, "cannot write Codex Desktop download");
        bytes = &bytes[written..];
        offset = offset
            .checked_add(written as u64)
            .context("Codex Desktop file offset overflow")?;
    }
    Ok(())
}

#[cfg(windows)]
fn write_at(file: &File, bytes: &[u8], offset: u64) -> std::io::Result<usize> {
    std::os::windows::fs::FileExt::seek_write(file, bytes, offset)
}

#[cfg(unix)]
fn write_at(file: &File, bytes: &[u8], offset: u64) -> std::io::Result<usize> {
    std::os::unix::fs::FileExt::write_at(file, bytes, offset)
}

#[cfg(test)]
async fn install_with<F, I>(
    client: &Client,
    endpoint: &MirrorEndpoint,
    installer: &I,
    progress: F,
) -> Result<()>
where
    F: Fn(MirrorProgress) + Send + Sync,
    I: PackageInstaller,
{
    install_target_with(
        client,
        endpoint,
        DesktopMirrorTarget::WindowsX64,
        installer,
        progress,
    )
    .await
}

fn prepare_partial(
    file: &mut File,
    parts: &[MirrorPart],
    control: &MirrorInstallControl,
) -> Result<(usize, u64)> {
    let existing_size = file.metadata()?.len();
    let mut valid_size = 0_u64;
    let mut start_part = 0_usize;
    for (index, part) in parts.iter().enumerate() {
        control.check()?;
        let Some(next_size) = valid_size.checked_add(part.size_bytes) else {
            break;
        };
        if next_size > existing_size {
            break;
        }
        let mut part_digest = Sha256::new();
        file.seek(SeekFrom::Start(valid_size))?;
        let mut remaining = part.size_bytes;
        let mut buffer = [0_u8; 64 * 1024];
        while remaining > 0 {
            let read_len = remaining.min(buffer.len() as u64) as usize;
            let read = file.read(&mut buffer[..read_len])?;
            if read == 0 {
                remaining = 1;
                break;
            }
            part_digest.update(&buffer[..read]);
            remaining -= read as u64;
        }
        if remaining != 0 || !digest_matches(&part_digest.finalize(), &part.sha256) {
            break;
        }
        valid_size = next_size;
        start_part = index + 1;
    }
    file.set_len(valid_size)?;
    file.seek(SeekFrom::End(0))?;
    Ok((start_part, valid_size))
}

fn hash_prefix(file: &mut File, length: u64, control: &MirrorInstallControl) -> Result<Sha256> {
    let mut digest = Sha256::new();
    let mut remaining = length;
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        control.check()?;
        let read_len = remaining.min(buffer.len() as u64) as usize;
        let read = file.read(&mut buffer[..read_len])?;
        ensure!(read > 0, "Codex Desktop partial download is truncated");
        digest.update(&buffer[..read]);
        remaining -= read as u64;
    }
    Ok(digest)
}

async fn fetch_manifest(client: &Client, url: &str) -> Result<Vec<u8>> {
    let mut last_error = None;
    for attempt in 1..=MAX_DOWNLOAD_ATTEMPTS {
        match fetch_manifest_once(client, url).await {
            Ok(bytes) => return Ok(bytes),
            Err(error) => {
                last_error = Some(error);
                if attempt == MAX_DOWNLOAD_ATTEMPTS {
                    break;
                }
                tokio::time::sleep(
                    DOWNLOAD_RETRY_BASE_DELAY
                        .checked_mul(attempt as u32)
                        .unwrap_or(DOWNLOAD_RETRY_BASE_DELAY),
                )
                .await;
            }
        }
    }
    Err(last_error.expect("a failed manifest attempt must provide an error"))
}

async fn fetch_manifest_once(client: &Client, url: &str) -> Result<Vec<u8>> {
    let mut response = get_exact(client, url, true).await?;
    if let Some(length) = response.content_length() {
        ensure!(
            length <= MAX_MANIFEST_BYTES as u64,
            "Codex Desktop mirror manifest is too large"
        );
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("Codex Desktop mirror manifest download failed")?
    {
        ensure!(
            bytes
                .len()
                .checked_add(chunk.len())
                .is_some_and(|size| size <= MAX_MANIFEST_BYTES),
            "Codex Desktop mirror manifest is too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn get_exact(client: &Client, expected: &str, no_cache: bool) -> Result<reqwest::Response> {
    let expected_url = Url::parse(expected)?;
    let mut request = client.get(expected_url.clone());
    if no_cache {
        request = request.header(reqwest::header::CACHE_CONTROL, "no-cache");
    }
    let response = request
        .send()
        .await
        .context("Codex Desktop mirror request failed")?;
    ensure!(
        response.status() == StatusCode::OK,
        "Codex Desktop mirror response is not OK"
    );
    ensure!(
        *response.url() == expected_url,
        "Codex Desktop mirror redirect is forbidden"
    );
    Ok(response)
}

async fn get_part(
    client: &Client,
    expected: &str,
    offset: u64,
    total: u64,
) -> Result<reqwest::Response> {
    let expected_url = Url::parse(expected)?;
    let mut request = client.get(expected_url.clone());
    if offset > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
    }
    let response = request
        .send()
        .await
        .context("Codex Desktop mirror request failed")?;
    ensure!(
        matches!(response.status(), StatusCode::OK | StatusCode::PARTIAL_CONTENT),
        "Codex Desktop mirror response is not OK"
    );
    ensure!(
        *response.url() == expected_url,
        "Codex Desktop mirror redirect is forbidden"
    );
    if response.status() == StatusCode::PARTIAL_CONTENT {
        ensure!(offset < total, "Codex Desktop mirror range starts past the part");
    }
    Ok(response)
}

fn validate_manifest<'a>(
    manifest: &'a MirrorManifest,
    endpoint: &MirrorEndpoint,
    target: DesktopMirrorTarget,
) -> Result<&'a MirrorAsset> {
    ensure!(
        manifest.schema_version == 3,
        "unsupported Codex Desktop mirror schema"
    );
    ensure!(
        manifest.mirror_provider == "cloudflare_r2",
        "untrusted Codex Desktop mirror provider"
    );
    ensure!(
        manifest.mirror_repository == "z8hk/codex-mirror",
        "untrusted Codex Desktop mirror repository"
    );
    ensure!(
        manifest.upstream_repository == "openai/codex",
        "untrusted Codex Desktop upstream"
    );
    ensure!(
        !manifest.tag.is_empty()
            && manifest.tag.len() <= 64
            && manifest.tag != "."
            && manifest.tag != ".."
            && manifest
                .tag
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_')),
        "invalid Codex Desktop mirror version path"
    );
    ensure!(
        !manifest.version.is_empty()
            && manifest.version.len() <= 64
            && manifest.generated_at.len() <= 64,
        "invalid Codex Desktop mirror metadata"
    );
    let upstream_release = Url::parse(&manifest.upstream_release_url)?;
    ensure!(
        upstream_release.scheme() == "https" && upstream_release.host_str() == Some("openai.com"),
        "untrusted Codex Desktop release URL"
    );
    ensure!(
        !manifest.assets.is_empty() && manifest.assets.len() <= 16,
        "invalid Codex Desktop mirror assets"
    );
    let mut matches = manifest
        .assets
        .iter()
        .filter(|asset| asset.filename == target.asset_filename());
    let asset = matches.next().ok_or(MissingMirrorAsset {
        target: target.as_str(),
    })?;
    ensure!(
        matches.next().is_none(),
        "duplicate Codex Desktop mirror target asset"
    );
    ensure!(
        asset.size_bytes > 0 && asset.size_bytes <= MAX_ARTIFACT_BYTES,
        "Codex Desktop artifact size is out of bounds"
    );
    validate_sha256(&asset.sha256)?;
    ensure!(
        asset.upstream_url == target.upstream_url()
            || (target.is_windows()
                && asset.upstream_url
                    == "https://get.microsoft.com/installer/download/9PLM9XGG6VKS"),
        "untrusted Codex Desktop asset source"
    );
    ensure!(
        !asset.parts.is_empty() && asset.parts.len() <= MAX_PARTS,
        "invalid Codex Desktop part count"
    );
    let mut sum = 0_u64;
    for (index, part) in asset.parts.iter().enumerate() {
        ensure!(
            part.filename == format!("{}.part-{index:04}", target.asset_filename()),
            "Codex Desktop parts are not in numeric order"
        );
        ensure!(
            part.mirror_url == endpoint.part_url(&manifest.tag, &part.filename),
            "untrusted Codex Desktop part URL"
        );
        ensure!(
            part.encoding == "identity" && part.decoded_size_bytes == part.size_bytes,
            "unsupported Codex Desktop part encoding"
        );
        ensure!(
            part.size_bytes > 0 && part.size_bytes <= MAX_PART_BYTES,
            "Codex Desktop part size is out of bounds"
        );
        validate_sha256(&part.sha256)?;
        sum = sum
            .checked_add(part.size_bytes)
            .context("Codex Desktop size overflow")?;
    }
    ensure!(
        sum == asset.size_bytes,
        "Codex Desktop part sizes do not match artifact"
    );
    Ok(asset)
}

fn validate_sha256(value: &str) -> Result<()> {
    ensure!(
        value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid Codex Desktop SHA-256"
    );
    Ok(())
}

fn digest_matches(bytes: &[u8], expected_hex: &str) -> bool {
    bytes
        .iter()
        .zip(expected_hex.as_bytes().chunks_exact(2))
        .all(|(byte, hex)| {
            u8::from_str_radix(std::str::from_utf8(hex).unwrap_or(""), 16).ok() == Some(*byte)
        })
}

struct DownloadPackage {
    root: PathBuf,
    partial: PathBuf,
    package: PathBuf,
}

impl DownloadPackage {
    fn open(cache_root: &Path, target: DesktopMirrorTarget, artifact_sha256: &str) -> Result<Self> {
        let root = cache_root.join(target.as_str()).join(artifact_sha256);
        fs::create_dir_all(&root).context("cannot create Codex Desktop download directory")?;
        Ok(Self {
            partial: root.join(format!("{}.partial", target.asset_filename())),
            package: root.join(target.asset_filename()),
            root,
        })
    }

    fn remove_completed(&self) {
        let _ = fs::remove_file(&self.partial);
        let _ = fs::remove_file(&self.package);
        let _ = fs::remove_dir(&self.root);
        if let Some(target_root) = self.root.parent() {
            let _ = fs::remove_dir(target_root);
        }
    }
}

fn preserve_failed_install(package: &DownloadPackage) {
    if !package.package.is_file() || package.partial.exists() {
        return;
    }
    if fs::rename(&package.package, &package.partial).is_ok() {
        return;
    }
    // A security scanner or Windows deployment service can briefly hold the
    // finalized file open. Copying gives the next attempt a resumable package
    // even when the atomic rename is temporarily unavailable.
    if fs::copy(&package.package, &package.partial).is_ok() {
        let _ = fs::remove_file(&package.package);
    }
}

impl Drop for DownloadPackage {
    fn drop(&mut self) {
        let keep_partial = fs::metadata(&self.partial)
            .map(|metadata| metadata.is_file() && metadata.len() > 0)
            .unwrap_or(false);
        if !keep_partial {
            let _ = fs::remove_file(&self.partial);
        }
        let _ = fs::remove_file(&self.package);
        if !keep_partial {
            let _ = fs::remove_dir(&self.root);
            if let Some(target_root) = self.root.parent() {
                let _ = fs::remove_dir(target_root);
            }
        }
    }
}

fn validate_msix_identity(path: &Path, target: DesktopMirrorTarget) -> Result<()> {
    let file = File::open(path).context("cannot open Codex Desktop package")?;
    let mut archive =
        zip::ZipArchive::new(file).context("Codex Desktop package is not a valid MSIX archive")?;
    ensure!(
        archive
            .file_names()
            .filter(|name| *name == "AppxManifest.xml")
            .count()
            == 1,
        "Codex Desktop package must contain one AppxManifest.xml"
    );
    let manifest = archive
        .by_name("AppxManifest.xml")
        .context("Codex Desktop package lacks AppxManifest.xml")?;
    ensure!(
        manifest.size() <= MAX_MSIX_MANIFEST_BYTES,
        "Codex Desktop package manifest is too large"
    );
    let mut xml = Vec::new();
    manifest
        .take(MAX_MSIX_MANIFEST_BYTES + 1)
        .read_to_end(&mut xml)
        .context("cannot read Codex Desktop package manifest")?;
    ensure!(
        xml.len() as u64 <= MAX_MSIX_MANIFEST_BYTES,
        "Codex Desktop package manifest is too large"
    );
    let mut reader = quick_xml::Reader::from_reader(xml.as_slice());
    let mut buf = Vec::new();
    let mut depth = 0_u32;
    let mut root_seen = false;
    let mut identity_seen = false;
    loop {
        match reader
            .read_event_into(&mut buf)
            .context("invalid Codex Desktop package XML")?
        {
            Event::Start(ref event) => {
                if depth == 0 {
                    ensure!(
                        !root_seen && event.name().as_ref() == b"Package",
                        "invalid Codex Desktop package root"
                    );
                    root_seen = true;
                } else if depth == 1 && event.name().as_ref() == b"Identity" {
                    ensure!(!identity_seen, "duplicate Codex Desktop package identity");
                    validate_identity_attributes(event, target)?;
                    identity_seen = true;
                }
                depth = depth
                    .checked_add(1)
                    .context("invalid Codex Desktop package nesting")?;
            }
            Event::Empty(ref event) => {
                if depth == 1 && event.name().as_ref() == b"Identity" {
                    ensure!(!identity_seen, "duplicate Codex Desktop package identity");
                    validate_identity_attributes(event, target)?;
                    identity_seen = true;
                }
            }
            Event::End(_) => {
                ensure!(depth > 0, "invalid Codex Desktop package XML nesting");
                depth -= 1;
            }
            Event::DocType(_) => bail!("Codex Desktop package XML doctype is forbidden"),
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    ensure!(
        root_seen && depth == 0 && identity_seen,
        "Codex Desktop package identity is missing"
    );
    Ok(())
}

fn validate_identity_attributes(
    event: &quick_xml::events::BytesStart<'_>,
    target: DesktopMirrorTarget,
) -> Result<()> {
    let mut name = None;
    let mut publisher = None;
    let mut architecture = None;
    for attribute in event.attributes() {
        let attribute = attribute.context("invalid Codex Desktop identity attribute")?;
        match attribute.key.as_ref() {
            b"Name" => name = Some(attribute.value.as_ref().to_vec()),
            b"Publisher" => publisher = Some(attribute.value.as_ref().to_vec()),
            b"ProcessorArchitecture" => architecture = Some(attribute.value.as_ref().to_vec()),
            _ => {}
        }
    }
    ensure!(
        name.as_deref() == Some(PACKAGE_NAME),
        "Codex Desktop package name mismatch"
    );
    ensure!(
        publisher.as_deref() == Some(PACKAGE_PUBLISHER),
        "Codex Desktop package publisher mismatch"
    );
    let expected_architecture: &[u8] = match target {
        DesktopMirrorTarget::WindowsX64 => b"x64",
        DesktopMirrorTarget::WindowsArm64 => b"arm64",
        _ => bail!("MSIX identity validation requires Windows target"),
    };
    ensure!(
        architecture.as_deref() == Some(expected_architecture),
        "Codex Desktop package architecture mismatch"
    );
    Ok(())
}

#[cfg(any(windows, test))]
const WINDOWS_ELEVATED_INSTALL_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'

function Write-Z8InstallResult {
    param(
        [string]$Status,
        [int]$ExitCode,
        [string]$HResult
    )
    $resultPath = $env:Z8_CODEX_DESKTOP_RESULT
    if ([string]::IsNullOrWhiteSpace($resultPath)) {
        return
    }
    $payload = [ordered]@{
        status = $Status
        exitCode = $ExitCode
        hresult = if ([string]::IsNullOrWhiteSpace($HResult)) { $null } else { $HResult }
    }
    [IO.File]::WriteAllText(
        $resultPath,
        ($payload | ConvertTo-Json -Compress),
        [Text.UTF8Encoding]::new($false)
    )
}

function Get-Z8HResult {
    param($ErrorRecord)
    $current = $ErrorRecord.Exception
    while ($null -ne $current) {
        if ($current.HResult -ne 0) {
            return ('0X{0}' -f ('{0:X8}' -f $current.HResult))
        }
        $current = $current.InnerException
    }
    return $null
}

$packages = @(Get-AppxPackage -Name 'OpenAI.Codex')
foreach ($package in $packages) {
    $location = [string]$package.InstallLocation
    $manifest = if ([string]::IsNullOrWhiteSpace($location)) { $null } else { Join-Path $location 'AppxManifest.xml' }
    if ([string]::IsNullOrWhiteSpace($location) -or -not (Test-Path -LiteralPath $manifest)) {
        Remove-AppxPackage -Package $package.PackageFullName -ErrorAction Stop
        continue
    }
    # A healthy package is already usable. The manager checks this before
    # starting installation, but treating it as success also makes the
    # command safe when two windows race.
    Write-Output 'Z8_CODEX_DESKTOP_ALREADY_INSTALLED'
    Write-Z8InstallResult 'ok' 0 $null
    exit 0
}
try {
    Add-AppxPackage -Path $env:Z8_CODEX_DESKTOP_MSIX -ErrorAction Stop
    Write-Z8InstallResult 'ok' 0 $null
} catch {
    Write-Z8InstallResult 'failed' 1 (Get-Z8HResult $_)
    throw
}
"#;

#[cfg(any(windows, test))]
const WINDOWS_INSTALL_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'

function Write-Z8InstallResult {
    param(
        [string]$Status,
        [int]$ExitCode,
        [string]$HResult
    )
    $resultPath = $env:Z8_CODEX_DESKTOP_RESULT
    if ([string]::IsNullOrWhiteSpace($resultPath)) {
        return
    }
    $payload = [ordered]@{
        status = $Status
        exitCode = $ExitCode
        hresult = if ([string]::IsNullOrWhiteSpace($HResult)) { $null } else { $HResult }
    }
    [IO.File]::WriteAllText(
        $resultPath,
        ($payload | ConvertTo-Json -Compress),
        [Text.UTF8Encoding]::new($false)
    )
}

function Test-Z8Elevated {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}
function Test-Z8AdminRequired {
    param($ErrorRecord)
    # PowerShell often keeps the deployment HRESULT on Exception.HResult
    # without rendering it in the error text. Check the exception chain as
    # well as the text so the UAC retry is not skipped on localized systems.
    $current = $ErrorRecord.Exception
    while ($null -ne $current) {
        if (('{0:X8}' -f $current.HResult) -eq '80073D28') {
            return $true
        }
        $current = $current.InnerException
    }
    return ([string]$ErrorRecord) -match '(?i)0x80073D28|ERROR_PACKAGED_SERVICE_REQUIRES_ADMIN_PRIVILEGES'
}
function Invoke-Z8Install {
    $packages = @(Get-AppxPackage -Name 'OpenAI.Codex')
    foreach ($package in $packages) {
        $location = [string]$package.InstallLocation
        $manifest = if ([string]::IsNullOrWhiteSpace($location)) { $null } else { Join-Path $location 'AppxManifest.xml' }
        if ([string]::IsNullOrWhiteSpace($location) -or -not (Test-Path -LiteralPath $manifest)) {
            Remove-AppxPackage -Package $package.PackageFullName -ErrorAction Stop
            continue
        }
        # A healthy package is already usable. The manager checks this before
        # starting installation, but treating it as success also makes the
        # command safe when two windows race.
        Write-Output 'Z8_CODEX_DESKTOP_ALREADY_INSTALLED'
        return
    }
    Add-AppxPackage -Path $env:Z8_CODEX_DESKTOP_MSIX -ErrorAction Stop
}
try {
    Invoke-Z8Install
} catch {
    # AppX deployment can require elevation for a packaged system service or
    # for removing a stale registration. Retry only for this exact Windows
    # deployment error; normal installation remains non-elevated.
    if (-not (Test-Z8Elevated) -and (Test-Z8AdminRequired $_)) {
        $child = Start-Process -FilePath 'powershell.exe' -Verb RunAs -WindowStyle Hidden -Wait -PassThru -ArgumentList @(
            '-NoLogo',
            '-NoProfile',
            '-NonInteractive',
            '-EncodedCommand',
            '__Z8_CODEX_DESKTOP_ELEVATED_COMMAND__'
        )
        if ($child.ExitCode -eq 0) {
            exit 0
        }
        if (-not [string]::IsNullOrWhiteSpace($env:Z8_CODEX_DESKTOP_RESULT) -and
            -not (Test-Path -LiteralPath $env:Z8_CODEX_DESKTOP_RESULT)) {
            Write-Z8InstallResult 'failed' $child.ExitCode '0X80073D28'
        }
        throw "Elevated Codex Desktop installation failed with exit code $($child.ExitCode)."
    }
    if (-not [string]::IsNullOrWhiteSpace($env:Z8_CODEX_DESKTOP_RESULT) -and
        -not (Test-Path -LiteralPath $env:Z8_CODEX_DESKTOP_RESULT)) {
        Write-Z8InstallResult 'failed' 1 $null
    }
    throw
}
"#;

#[cfg(windows)]
async fn install_verified_msix(path: &Path) -> Result<()> {
    // The package path is supplied only through an environment variable. No
    // untrusted path is ever interpolated into PowerShell source. Windows
    // validates the MSIX signature and publisher as part of Add-AppxPackage.
    let package_token = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        path.to_string_lossy().as_bytes(),
    );
    let result_path = std::env::temp_dir().join(format!(
        "z8-codex-desktop-install-{}.json",
        uuid::Uuid::new_v4()
    ));
    let _ = fs::remove_file(&result_path);
    let result_token = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        result_path.to_string_lossy().as_bytes(),
    );
    let elevated_script = format!(
        "$env:Z8_CODEX_DESKTOP_MSIX = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{package_token}'))\n$env:Z8_CODEX_DESKTOP_RESULT = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{result_token}'))\n{WINDOWS_ELEVATED_INSTALL_SCRIPT}"
    );
    let elevated_encoded = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        elevated_script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    let install_script = WINDOWS_INSTALL_SCRIPT.replace(ELEVATED_COMMAND_MARKER, &elevated_encoded);
    let encoded = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        install_script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    let output = tokio::process::Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
            &encoded,
        ])
        .env("Z8_CODEX_DESKTOP_MSIX", path)
        .env("Z8_CODEX_DESKTOP_RESULT", &result_path)
        .creation_flags(crate::windows_create_no_window())
        .output()
        .await;
    let result_file = fs::read_to_string(&result_path).ok();
    let _ = fs::remove_file(&result_path);
    let output = output.context("cannot start Windows package installer")?;
    if !output.status.success() {
        let elevated_result = result_file
            .as_deref()
            .and_then(|value| serde_json::from_str::<WindowsInstallResult>(value).ok());
        let hresult = elevated_result
            .as_ref()
            .and_then(|value| value.hresult.clone())
            .or_else(|| extract_hresult(&output.stderr))
            .or_else(|| extract_hresult(&output.stdout));
        let exit_code = elevated_result
            .and_then(|value| value.exit_code)
            .or_else(|| output.status.code());
        let _ = crate::diagnostic_log::append_diagnostic_log(
            "codex_desktop_mirror.windows_install_failed",
            serde_json::json!({
                "exit_code": exit_code,
                "hresult": hresult,
                "stderr_bytes": output.stderr.len(),
                "stdout_bytes": output.stdout.len(),
            }),
        );
        return Err(WindowsPackageInstallError {
            exit_code,
            hresult,
        }
        .into());
    }
    Ok(())
}

#[cfg(windows)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WindowsInstallResult {
    #[allow(dead_code)]
    status: String,
    exit_code: Option<i32>,
    hresult: Option<String>,
}

#[cfg(any(windows, test))]
fn extract_hresult(output: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(output);
    text.split(|character: char| {
        !character.is_ascii_hexdigit() && character != 'x' && character != 'X'
    })
    .find_map(|token| {
        if token.len() != 10 || !token[..2].eq_ignore_ascii_case("0x") {
            return None;
        }
        token[2..]
            .chars()
            .all(|character| character.is_ascii_hexdigit())
            .then(|| token.to_ascii_uppercase())
    })
}

#[cfg(not(windows))]
async fn install_verified_msix(_path: &Path) -> Result<()> {
    bail!("Codex Desktop MSIX installation requires Windows")
}

#[cfg(any(target_os = "macos", test))]
fn find_codex_app_in_mount(mountpoint: &Path) -> Result<PathBuf> {
    let mut found = None;
    for name in ["Codex.app", "ChatGPT.app"] {
        let candidate = mountpoint.join(name);
        match candidate.symlink_metadata() {
            Ok(metadata) => {
                ensure!(
                    metadata.file_type().is_dir(),
                    "Codex Desktop DMG app is not a regular directory"
                );
                ensure!(
                    found.is_none(),
                    "Codex Desktop DMG has ambiguous app bundles"
                );
                found = Some(candidate);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("cannot inspect Codex Desktop DMG app"),
        }
    }
    if let Some(found) = found {
        return Ok(found);
    }
    // The mirror publisher prefers Codex.app, then accepts exactly one app
    // bundle under the image root after validating its signing identity. Keep
    // that rename-tolerant behavior; the caller repeats signing and bundle-ID
    // checks before the app is copied.
    for entry in fs::read_dir(mountpoint).context("cannot list Codex Desktop DMG")? {
        let entry = entry.context("cannot inspect Codex Desktop DMG entry")?;
        let candidate = entry.path();
        if candidate
            .extension()
            .is_none_or(|extension| extension != "app")
        {
            continue;
        }
        ensure!(
            entry
                .file_type()
                .context("cannot inspect Codex Desktop DMG app type")?
                .is_dir(),
            "Codex Desktop DMG app is not a regular directory"
        );
        ensure!(
            found.is_none(),
            "Codex Desktop DMG has ambiguous app bundles"
        );
        found = Some(candidate);
    }
    found.context("Codex Desktop DMG has no app bundle")
}

#[cfg(target_os = "macos")]
async fn install_verified_dmg(path: &Path, target: DesktopMirrorTarget) -> Result<()> {
    use std::process::Command;

    fn run(program: &str, arguments: &[&std::ffi::OsStr]) -> Result<()> {
        let status = Command::new(program)
            .args(arguments)
            .status()
            .with_context(|| format!("cannot start {program}"))?;
        ensure!(
            status.success(),
            "Codex Desktop macOS package verification or installation failed"
        );
        Ok(())
    }
    fn read(program: &str, arguments: &[&std::ffi::OsStr]) -> Result<String> {
        let output = Command::new(program)
            .args(arguments)
            .output()
            .with_context(|| format!("cannot start {program}"))?;
        ensure!(
            output.status.success(),
            "Codex Desktop macOS bundle inspection failed"
        );
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    }
    fn valid_bundle_component(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
            && value != "."
            && value != ".."
    }

    let path_os = path.as_os_str();
    run(
        "/usr/bin/hdiutil",
        &["verify".as_ref(), "-quiet".as_ref(), path_os],
    )?;
    let mountpoint = path
        .parent()
        .context("Codex Desktop temporary directory missing")?
        .join("mounted");
    fs::create_dir(&mountpoint).context("cannot create Codex Desktop mountpoint")?;
    let mut mount = MountedDmg {
        mountpoint,
        attached: false,
    };
    mount.attached = true;
    run(
        "/usr/bin/hdiutil",
        &[
            "attach".as_ref(),
            path_os,
            "-nobrowse".as_ref(),
            "-readonly".as_ref(),
            "-quiet".as_ref(),
            "-mountpoint".as_ref(),
            mount.mountpoint.as_os_str(),
        ],
    )?;

    // The publisher verifies the bundle, signing identity, and native
    // architecture before mirroring. Repeat those checks locally after hash
    // verification and before touching the user's Applications directory.
    let app = find_codex_app_in_mount(&mount.mountpoint)?;
    let requirement = "identifier \"com.openai.codex\" and anchor apple generic and certificate leaf[subject.OU] = \"2DC432GLL2\"";
    run(
        "/usr/bin/codesign",
        &[
            "--verify".as_ref(),
            "--deep".as_ref(),
            "--strict".as_ref(),
            format!("-R={requirement}").as_ref(),
            app.as_os_str(),
        ],
    )?;
    let plist = app.join("Contents/Info.plist");
    let bundle_id = read(
        "/usr/libexec/PlistBuddy",
        &[
            "-c".as_ref(),
            "Print :CFBundleIdentifier".as_ref(),
            plist.as_os_str(),
        ],
    )?;
    ensure!(
        bundle_id == "com.openai.codex",
        "Codex Desktop bundle identifier mismatch"
    );
    let executable = read(
        "/usr/libexec/PlistBuddy",
        &[
            "-c".as_ref(),
            "Print :CFBundleExecutable".as_ref(),
            plist.as_os_str(),
        ],
    )?;
    ensure!(
        valid_bundle_component(&executable),
        "invalid Codex Desktop bundle executable"
    );
    let binary = app.join("Contents/MacOS").join(executable);
    ensure!(binary.is_file(), "Codex Desktop bundle executable missing");
    let archs = read("/usr/bin/lipo", &["-archs".as_ref(), binary.as_os_str()])?;
    let required_arch = match target {
        DesktopMirrorTarget::MacosX64 => "x86_64",
        DesktopMirrorTarget::MacosArm64 => "arm64",
        _ => bail!("DMG installation requires macOS target"),
    };
    ensure!(
        archs.split_whitespace().any(|arch| arch == required_arch),
        "Codex Desktop app architecture mismatch"
    );

    ensure!(
        crate::app_paths::resolve_codex_app_dir(None).is_none(),
        "Codex Desktop is already installed"
    );
    let home = directories::BaseDirs::new()
        .context("cannot determine macOS user Applications directory")?
        .home_dir()
        .to_path_buf();
    let applications = home.join("Applications");
    fs::create_dir_all(&applications).context("cannot create user Applications directory")?;
    let destination = applications.join("Codex.app");
    fs::create_dir(&destination)
        .context("Codex Desktop is already installed or destination is unavailable")?;
    let mut new_app = NewMacApp {
        path: destination,
        installed: false,
    };
    run(
        "/usr/bin/ditto",
        &[app.as_os_str(), new_app.path.as_os_str()],
    )?;
    ensure!(
        new_app.path.join("Contents/Info.plist").is_file()
            && new_app
                .path
                .join("Contents/MacOS")
                .join(
                    binary
                        .file_name()
                        .context("Codex Desktop executable missing")?
                )
                .is_file(),
        "installed Codex Desktop bundle is incomplete"
    );
    let copied_bundle_id = read(
        "/usr/libexec/PlistBuddy",
        &[
            "-c".as_ref(),
            "Print :CFBundleIdentifier".as_ref(),
            new_app.path.join("Contents/Info.plist").as_os_str(),
        ],
    )?;
    ensure!(
        copied_bundle_id == "com.openai.codex",
        "installed Codex Desktop bundle identifier mismatch"
    );
    let copied_archs = read(
        "/usr/bin/lipo",
        &[
            "-archs".as_ref(),
            new_app
                .path
                .join("Contents/MacOS")
                .join(
                    binary
                        .file_name()
                        .context("Codex Desktop executable missing")?,
                )
                .as_os_str(),
        ],
    )?;
    ensure!(
        copied_archs
            .split_whitespace()
            .any(|arch| arch == required_arch),
        "installed Codex Desktop app architecture mismatch"
    );
    run(
        "/usr/bin/codesign",
        &[
            "--verify".as_ref(),
            "--deep".as_ref(),
            "--strict".as_ref(),
            format!("-R={requirement}").as_ref(),
            new_app.path.as_os_str(),
        ],
    )?;
    // Successful installation must also release the verified image. If
    // detaching fails, NewMacApp rolls back this newly created destination;
    // MountedDmg still retries the detach during Drop.
    mount.detach_checked()?;
    new_app.installed = true;
    Ok(())
}

#[cfg(target_os = "macos")]
struct MountedDmg {
    mountpoint: PathBuf,
    attached: bool,
}

#[cfg(target_os = "macos")]
impl MountedDmg {
    fn detach_checked(&mut self) -> Result<()> {
        if self.attached {
            let status = std::process::Command::new("/usr/bin/hdiutil")
                .args(["detach", "-quiet"])
                .arg(&self.mountpoint)
                .status()
                .context("cannot detach Codex Desktop disk image")?;
            ensure!(status.success(), "cannot detach Codex Desktop disk image");
            self.attached = false;
        }
        match fs::remove_dir(&self.mountpoint) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("cannot remove Codex Desktop mountpoint"),
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
impl Drop for MountedDmg {
    fn drop(&mut self) {
        if self.attached {
            let _ = std::process::Command::new("/usr/bin/hdiutil")
                .args(["detach", "-quiet"])
                .arg(&self.mountpoint)
                .status();
        }
        let _ = fs::remove_dir(&self.mountpoint);
    }
}

#[cfg(target_os = "macos")]
struct NewMacApp {
    path: PathBuf,
    installed: bool,
}

#[cfg(target_os = "macos")]
impl Drop for NewMacApp {
    fn drop(&mut self) {
        if !self.installed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(not(target_os = "macos"))]
async fn install_verified_dmg(_path: &Path, _target: DesktopMirrorTarget) -> Result<()> {
    bail!("Codex Desktop DMG installation requires macOS")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::{
        io::{Cursor, Write},
        sync::{
            atomic::{AtomicUsize, Ordering},
            Mutex,
        },
    };
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    #[test]
    fn finds_current_chatgpt_bundle_name() {
        let mount = tempfile::tempdir().unwrap();
        let app = mount.path().join("ChatGPT.app");
        fs::create_dir(&app).unwrap();
        assert_eq!(find_codex_app_in_mount(mount.path()).unwrap(), app);
    }

    #[test]
    fn rejects_ambiguous_codex_bundle_candidates() {
        let mount = tempfile::tempdir().unwrap();
        fs::create_dir(mount.path().join("Codex.app")).unwrap();
        fs::create_dir(mount.path().join("ChatGPT.app")).unwrap();
        assert!(find_codex_app_in_mount(mount.path()).is_err());
    }

    #[test]
    fn rejects_non_directory_bundle_candidate() {
        let mount = tempfile::tempdir().unwrap();
        fs::write(mount.path().join("ChatGPT.app"), b"not a bundle").unwrap();
        assert!(find_codex_app_in_mount(mount.path()).is_err());
    }

    #[test]
    fn accepts_one_future_bundle_name_for_later_signature_check() {
        let mount = tempfile::tempdir().unwrap();
        let app = mount.path().join("OpenAI Desktop.app");
        fs::create_dir(&app).unwrap();
        assert_eq!(find_codex_app_in_mount(mount.path()).unwrap(), app);
    }

    #[test]
    fn cancelled_install_control_cannot_enter_system_installation() {
        let control = MirrorInstallControl::default();
        assert!(control.check().is_ok());
        assert!(control.cancel());
        assert!(control.check().is_err());
        assert!(control.begin_installation().is_err());
        assert!(control.cancel());
    }

    #[test]
    fn partial_download_keeps_only_complete_verified_parts() {
        let first = b"verified first part";
        let second = b"part still downloading";
        let mut partial = tempfile::NamedTempFile::new().unwrap();
        partial.write_all(first).unwrap();
        partial.write_all(&second[..5]).unwrap();
        partial.as_file_mut().sync_all().unwrap();
        let parts = vec![
            MirrorPart {
                filename: "part-0000".into(),
                mirror_url: "https://download.z8.hk/part-0000".into(),
                size_bytes: first.len() as u64,
                sha256: digest(first),
                encoding: "identity".into(),
                decoded_size_bytes: first.len() as u64,
            },
            MirrorPart {
                filename: "part-0001".into(),
                mirror_url: "https://download.z8.hk/part-0001".into(),
                size_bytes: second.len() as u64,
                sha256: digest(second),
                encoding: "identity".into(),
                decoded_size_bytes: second.len() as u64,
            },
        ];
        let (start_part, completed) =
            prepare_partial(partial.as_file_mut(), &parts, &MirrorInstallControl::default())
                .unwrap();
        assert_eq!(start_part, 1);
        assert_eq!(completed, first.len() as u64);
        assert_eq!(partial.as_file().metadata().unwrap().len(), first.len() as u64);
    }

    #[test]
    fn rejects_ambiguous_future_bundle_names() {
        let mount = tempfile::tempdir().unwrap();
        fs::create_dir(mount.path().join("OpenAI Desktop.app")).unwrap();
        fs::create_dir(mount.path().join("Other.app")).unwrap();
        assert!(find_codex_app_in_mount(mount.path()).is_err());
    }

    struct RecordingInstaller {
        calls: AtomicUsize,
        package_path: Mutex<Option<PathBuf>>,
        fail: bool,
    }

    #[async_trait]
    impl PackageInstaller for RecordingInstaller {
        async fn install(&self, path: &Path) -> Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            ensure!(path.exists(), "fixture package was removed before install");
            *self.package_path.lock().unwrap() = Some(path.to_path_buf());
            if self.fail {
                bail!("mock install failed")
            }
            Ok(())
        }
    }

    impl RecordingInstaller {
        fn new(fail: bool) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                package_path: Mutex::new(None),
                fail,
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    fn msix(name: &str, publisher: &str) -> Vec<u8> {
        msix_arch(name, publisher, "x64")
    }

    fn msix_arch(name: &str, publisher: &str, architecture: &str) -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut bytes);
            writer
                .start_file("AppxManifest.xml", zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(format!(
                "<?xml version=\"1.0\"?><Package xmlns=\"http://schemas.microsoft.com/appx/manifest/foundation/windows10\"><Identity Name=\"{name}\" Publisher=\"{publisher}\" ProcessorArchitecture=\"{architecture}\" Version=\"1.0.0.0\" /></Package>"
            ).as_bytes()).unwrap();
            writer.finish().unwrap();
        }
        bytes.into_inner()
    }

    fn digest(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn fixture_manifest(endpoint: &MirrorEndpoint, bytes: &[u8]) -> Value {
        let split = bytes.len() / 2;
        let chunks = [&bytes[..split], &bytes[split..]];
        let parts: Vec<_> = chunks
            .iter()
            .enumerate()
            .map(|(index, part)| {
                let filename = format!("{ASSET_FILENAME}.part-{index:04}");
                json!({
                    "filename": filename,
                    "mirrorUrl": endpoint.part_url("26.924.2738.0", &filename),
                    "sizeBytes": part.len(),
                    "sha256": digest(part),
                    "encoding": "identity",
                    "decodedSizeBytes": part.len(),
                })
            })
            .collect();
        json!({
            "schemaVersion": 3,
            "mirrorProvider": "cloudflare_r2",
            "upstreamRepository": "openai/codex",
            "tag": "26.924.2738.0",
            "version": "26.924.2738.0",
            "upstreamReleaseUrl": "https://openai.com/codex/for-work/",
            "mirrorRepository": "z8hk/codex-mirror",
            "generatedAt": "2026-09-26T12:33:00Z",
            "assets": [{
                "filename": ASSET_FILENAME,
                "upstreamUrl": "https://get.microsoft.com/installer/download/9PLM9XGG6VKS",
                "sizeBytes": bytes.len(),
                "sha256": digest(bytes),
                "parts": parts,
            }]
        })
    }

    async fn fixture_server(
        bytes: &[u8],
        manifest_mutator: impl FnOnce(&mut Value),
    ) -> (Client, MirrorEndpoint, MockServer) {
        fixture_server_for_target(DesktopMirrorTarget::WindowsX64, bytes, manifest_mutator).await
    }

    async fn fixture_server_for_target(
        target: DesktopMirrorTarget,
        bytes: &[u8],
        manifest_mutator: impl FnOnce(&mut Value),
    ) -> (Client, MirrorEndpoint, MockServer) {
        let server = MockServer::start().await;
        let endpoint = MirrorEndpoint {
            origin: format!("{}/", server.uri()),
        };
        let mut manifest = fixture_manifest(&endpoint, bytes);
        manifest["assets"][0]["filename"] = target.asset_filename().into();
        manifest["assets"][0]["upstreamUrl"] = target.upstream_url().into();
        for (index, part) in manifest["assets"][0]["parts"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
        {
            let filename = format!("{}.part-{index:04}", target.asset_filename());
            part["filename"] = filename.clone().into();
            part["mirrorUrl"] = endpoint.part_url("26.924.2738.0", &filename).into();
        }
        manifest_mutator(&mut manifest);
        Mock::given(method("GET"))
            .and(path(format!("/{MIRROR_PATH}latest.json")))
            .respond_with(ResponseTemplate::new(200).set_body_json(manifest))
            .mount(&server)
            .await;
        let split = bytes.len() / 2;
        for (index, chunk) in [&bytes[..split], &bytes[split..]].iter().enumerate() {
            Mock::given(method("GET"))
                .and(path(format!(
                    "/{MIRROR_PATH}26.924.2738.0/{}.part-{index:04}",
                    target.asset_filename()
                )))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(*chunk))
                .mount(&server)
                .await;
        }
        let client = Client::builder().redirect(Policy::none()).build().unwrap();
        (client, endpoint, server)
    }

    #[tokio::test]
    async fn verified_parts_install_once_and_delete_temporary_package() {
        let bytes = msix("OpenAI.Codex", "CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B");
        let (client, endpoint, _server) = fixture_server(&bytes, |_| {}).await;
        let installer = RecordingInstaller::new(false);
        let events = Mutex::new(Vec::new());
        install_with(&client, &endpoint, &installer, |event| {
            events.lock().unwrap().push(event)
        })
        .await
        .unwrap();
        assert_eq!(installer.calls(), 1);
        let events = events.into_inner().unwrap();
        assert_eq!(events.first().unwrap().phase, MirrorPhase::Checking);
        assert_eq!(events.last().unwrap().phase, MirrorPhase::Complete);
        assert_eq!(events.last().unwrap().completed, bytes.len() as u64);
        assert!(!installer
            .package_path
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .exists());
    }

    #[tokio::test]
    async fn transient_part_failure_is_retried_before_installing() {
        let bytes = msix("OpenAI.Codex", "CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B");
        let (client, endpoint, server) = fixture_server(&bytes, |_| {}).await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/{MIRROR_PATH}26.924.2738.0/{ASSET_FILENAME}.part-0000"
            )))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        let installer = RecordingInstaller::new(false);
        install_with(&client, &endpoint, &installer, |_| {})
            .await
            .expect("the transient part failure should be retried");
        assert_eq!(installer.calls(), 1);
    }

    #[tokio::test]
    async fn four_targets_download_only_their_exact_parts_before_mock_install() {
        for target in [
            DesktopMirrorTarget::WindowsX64,
            DesktopMirrorTarget::WindowsArm64,
            DesktopMirrorTarget::MacosX64,
            DesktopMirrorTarget::MacosArm64,
        ] {
            let bytes = if target.is_windows() {
                let architecture = if target == DesktopMirrorTarget::WindowsArm64 {
                    "arm64"
                } else {
                    "x64"
                };
                msix_arch(
                    "OpenAI.Codex",
                    "CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B",
                    architecture,
                )
            } else {
                b"mock dmg bytes".to_vec()
            };
            let (client, endpoint, _server) =
                fixture_server_for_target(target, &bytes, |_| {}).await;
            let installer = RecordingInstaller::new(false);
            install_target_with(&client, &endpoint, target, &installer, |_| {})
                .await
                .unwrap();
            assert_eq!(installer.calls(), 1, "{}", target.as_str());
        }
    }

    #[tokio::test]
    async fn wrong_part_hash_does_not_call_installer() {
        let bytes = msix("OpenAI.Codex", "CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B");
        let (client, endpoint, _server) = fixture_server(&bytes, |manifest| {
            manifest["assets"][0]["parts"][1]["sha256"] = "0".repeat(64).into();
        })
        .await;
        let installer = RecordingInstaller::new(false);
        assert!(install_with(&client, &endpoint, &installer, |_| {})
            .await
            .unwrap_err()
            .to_string()
            .contains("digest mismatch"));
        assert_eq!(installer.calls(), 0);
    }

    #[tokio::test]
    async fn wrong_total_hash_or_size_does_not_call_installer() {
        let bytes = msix("OpenAI.Codex", "CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B");
        for alter in [0, 1] {
            let (client, endpoint, _server) = fixture_server(&bytes, |manifest| {
                if alter == 0 {
                    manifest["assets"][0]["sha256"] = "0".repeat(64).into();
                } else {
                    manifest["assets"][0]["sizeBytes"] = (bytes.len() + 1).into();
                }
            })
            .await;
            let installer = RecordingInstaller::new(false);
            assert!(install_with(&client, &endpoint, &installer, |_| {})
                .await
                .is_err());
            assert_eq!(installer.calls(), 0);
        }
    }

    #[tokio::test]
    async fn part_body_larger_than_declared_is_rejected_before_install() {
        let bytes = msix("OpenAI.Codex", "CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B");
        let (client, endpoint, _server) = fixture_server(&bytes, |manifest| {
            let part_size = manifest["assets"][0]["parts"][0]["sizeBytes"]
                .as_u64()
                .unwrap();
            manifest["assets"][0]["parts"][0]["sizeBytes"] = (part_size - 1).into();
            manifest["assets"][0]["parts"][0]["decodedSizeBytes"] = (part_size - 1).into();
            manifest["assets"][0]["sizeBytes"] = ((bytes.len() as u64) - 1).into();
        })
        .await;
        let installer = RecordingInstaller::new(false);
        assert!(install_with(&client, &endpoint, &installer, |_| {})
            .await
            .is_err());
        assert_eq!(installer.calls(), 0);
    }

    #[tokio::test]
    async fn untrusted_url_or_order_never_installs() {
        let bytes = msix("OpenAI.Codex", "CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B");
        for alter in 0..4 {
            let (client, endpoint, _server) = fixture_server(&bytes, |manifest| match alter {
                0 => {
                    manifest["assets"][0]["parts"][0]["mirrorUrl"] =
                        "https://evil.example/part".into()
                }
                1 => {
                    manifest["assets"][0]["parts"][0]["filename"] =
                        format!("{ASSET_FILENAME}.part-0001").into()
                }
                2 => {
                    manifest["assets"][0]["parts"][0]["mirrorUrl"] = manifest["assets"][0]["parts"]
                        [0]["mirrorUrl"]
                        .as_str()
                        .unwrap()
                        .replace("/26.924.2738.0/", "/../")
                        .into()
                }
                _ => {
                    manifest["assets"][0]["parts"][0]["mirrorUrl"] = format!(
                        "{}?next=https://evil.example",
                        manifest["assets"][0]["parts"][0]["mirrorUrl"]
                            .as_str()
                            .unwrap()
                    )
                    .into()
                }
            })
            .await;
            let installer = RecordingInstaller::new(false);
            assert!(install_with(&client, &endpoint, &installer, |_| {})
                .await
                .is_err());
            assert_eq!(installer.calls(), 0);
        }
    }

    #[tokio::test]
    async fn http_redirect_to_other_origin_never_installs() {
        let bytes = msix("OpenAI.Codex", "CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B");
        let (client, endpoint, server) = fixture_server(&bytes, |_| {}).await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/{MIRROR_PATH}26.924.2738.0/{ASSET_FILENAME}.part-0000"
            )))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("Location", "https://evil.example/package"),
            )
            .with_priority(1)
            .mount(&server)
            .await;
        let installer = RecordingInstaller::new(false);
        assert!(install_with(&client, &endpoint, &installer, |_| {})
            .await
            .is_err());
        assert_eq!(installer.calls(), 0);
    }

    #[tokio::test]
    async fn wrong_package_identity_and_mock_failure_clean_up() {
        for (name, publisher, fail) in [
            (
                "Fake.Codex",
                "CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B",
                false,
            ),
            ("OpenAI.Codex", "CN=Fake", false),
            (
                "OpenAI.Codex",
                "CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B",
                true,
            ),
        ] {
            let bytes = msix(name, publisher);
            let (client, endpoint, _server) = fixture_server(&bytes, |_| {}).await;
            let installer = RecordingInstaller::new(fail);
            assert!(install_with(&client, &endpoint, &installer, |_| {})
                .await
                .is_err());
            assert_eq!(installer.calls(), usize::from(fail));
            if fail {
                assert!(!installer
                    .package_path
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .exists());
            }
        }
    }

    #[tokio::test]
    async fn failed_install_keeps_verified_package_for_install_only_retry() {
        let bytes = msix("OpenAI.Codex", "CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B");
        let (client, endpoint, server) = fixture_server(&bytes, |_| {}).await;
        let cache = tempfile::tempdir().unwrap();
        let failed = RecordingInstaller::new(true);
        assert!(install_target_with_cancel(
            &client,
            &endpoint,
            DesktopMirrorTarget::WindowsX64,
            &failed,
            |_| {},
            &MirrorInstallControl::default(),
            cache.path(),
        )
        .await
        .is_err());
        let partial = cache
            .path()
            .join(DesktopMirrorTarget::WindowsX64.as_str())
            .join(digest(&bytes))
            .join(format!("{ASSET_FILENAME}.partial"));
        assert!(partial.is_file());
        let part_requests_before_retry = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.url.path().contains(".part-"))
            .count();
        assert_eq!(part_requests_before_retry, 2);

        let successful = RecordingInstaller::new(false);
        install_target_with_cancel(
            &client,
            &endpoint,
            DesktopMirrorTarget::WindowsX64,
            &successful,
            |_| {},
            &MirrorInstallControl::default(),
            cache.path(),
        )
        .await
        .unwrap();
        let part_requests_after_retry = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.url.path().contains(".part-"))
            .count();
        assert_eq!(part_requests_after_retry, part_requests_before_retry);
        assert!(!partial.exists());
    }

    #[test]
    fn windows_installer_repairs_stale_registration_instead_of_failing_as_already_installed() {
        assert!(WINDOWS_INSTALL_SCRIPT.contains("Get-AppxPackage -Name 'OpenAI.Codex'"));
        assert!(WINDOWS_INSTALL_SCRIPT.contains("Remove-AppxPackage"));
        assert!(WINDOWS_INSTALL_SCRIPT.contains("AppxManifest.xml"));
        assert!(WINDOWS_INSTALL_SCRIPT.contains("Add-AppxPackage"));
        assert!(WINDOWS_INSTALL_SCRIPT.contains("Start-Process -FilePath 'powershell.exe' -Verb RunAs"));
        assert!(WINDOWS_INSTALL_SCRIPT.contains("0x80073D28"));
        assert!(WINDOWS_INSTALL_SCRIPT.contains("$ErrorRecord.Exception"));
        assert!(WINDOWS_INSTALL_SCRIPT.contains("{0:X8}"));
        assert!(WINDOWS_INSTALL_SCRIPT.contains("Z8_CODEX_DESKTOP_RESULT"));
        assert!(WINDOWS_INSTALL_SCRIPT.contains("__Z8_CODEX_DESKTOP_ELEVATED_COMMAND__"));
        assert!(WINDOWS_ELEVATED_INSTALL_SCRIPT.contains("Write-Z8InstallResult"));
        assert!(WINDOWS_ELEVATED_INSTALL_SCRIPT.contains("Remove-AppxPackage"));
        assert!(!WINDOWS_INSTALL_SCRIPT.contains("throw 'Codex Desktop is already installed'"));
    }

    #[test]
    fn admin_required_hresult_is_classified_without_exposing_output() {
        let error = WindowsPackageInstallError {
            exit_code: Some(1),
            hresult: Some("0X80073D28".into()),
        };
        assert!(error.requires_admin());
        assert!(!WindowsPackageInstallError {
            exit_code: Some(1),
            hresult: Some("0X80073CFB".into()),
        }
        .requires_admin());
        assert!(WindowsPackageInstallError {
            exit_code: Some(1223),
            hresult: None,
        }
        .cancelled());
        assert!(WindowsPackageInstallError {
            exit_code: Some(1),
            hresult: Some("0X800704C7".into()),
        }
        .cancelled());
    }

    #[test]
    fn extracts_windows_hresult_without_logging_local_paths() {
        assert_eq!(
            extract_hresult(b"Deployment failed with HRESULT 0x80073CFB."),
            Some("0X80073CFB".into())
        );
        assert_eq!(extract_hresult(b"no HRESULT"), None);
    }

    #[test]
    fn production_entry_is_pinned_to_public_origin() {
        let endpoint = MirrorEndpoint::production();
        assert_eq!(endpoint.manifest_url(), CODEX_DESKTOP_MIRROR_MANIFEST_URL);
    }

    #[test]
    fn native_environment_selects_only_matching_platform_and_architecture() {
        for (platform, architecture, expected) in [
            ("windows", "x86_64", DesktopMirrorTarget::WindowsX64),
            ("windows", "aarch64", DesktopMirrorTarget::WindowsArm64),
            ("macos", "x86_64", DesktopMirrorTarget::MacosX64),
            ("macos", "aarch64", DesktopMirrorTarget::MacosArm64),
        ] {
            let environment = select_environment(platform, Some(architecture), true);
            assert_eq!(environment.target, Some(expected));
            assert_eq!(environment.reason, None);
            assert_eq!(
                expected.asset_filename(),
                format!(
                    "Codex-{}.{}",
                    expected.as_str(),
                    if platform == "windows" { "msix" } else { "dmg" }
                )
            );
        }
        assert_eq!(
            select_environment("windows", Some("x86_64"), false).reason,
            Some(DesktopMirrorUnsupportedReason::OsVersionTooOld)
        );
        assert_eq!(
            select_environment("macos", None, true).reason,
            Some(DesktopMirrorUnsupportedReason::ArchitectureUnknown)
        );
        assert_eq!(
            select_environment("linux", Some("x86_64"), true).reason,
            Some(DesktopMirrorUnsupportedReason::UnsupportedPlatform)
        );
    }

    #[test]
    fn manifest_selects_four_exact_assets_and_rejects_missing_or_cross_architecture_source() {
        let endpoint = MirrorEndpoint::production();
        let bytes = msix("OpenAI.Codex", "CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B");
        let mut manifest = fixture_manifest(&endpoint, &bytes);
        let targets = [
            DesktopMirrorTarget::WindowsX64,
            DesktopMirrorTarget::WindowsArm64,
            DesktopMirrorTarget::MacosX64,
            DesktopMirrorTarget::MacosArm64,
        ];
        let mut assets = Vec::new();
        for target in targets {
            let mut asset = manifest["assets"][0].clone();
            asset["filename"] = target.asset_filename().into();
            asset["upstreamUrl"] = target.upstream_url().into();
            for (index, part) in asset["parts"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .enumerate()
            {
                let filename = format!("{}.part-{index:04}", target.asset_filename());
                part["filename"] = filename.clone().into();
                part["mirrorUrl"] = endpoint.part_url("26.924.2738.0", &filename).into();
            }
            assets.push(asset);
        }
        manifest["assets"] = Value::Array(assets);
        let parsed: MirrorManifest = serde_json::from_value(manifest.clone()).unwrap();
        for target in targets {
            assert_eq!(
                validate_manifest(&parsed, &endpoint, target)
                    .unwrap()
                    .filename,
                target.asset_filename()
            );
        }
        manifest["assets"]
            .as_array_mut()
            .unwrap()
            .retain(|asset| asset["filename"] != DesktopMirrorTarget::MacosArm64.asset_filename());
        let parsed: MirrorManifest = serde_json::from_value(manifest.clone()).unwrap();
        let error =
            validate_manifest(&parsed, &endpoint, DesktopMirrorTarget::MacosArm64).unwrap_err();
        assert_eq!(
            error.downcast_ref::<MissingMirrorAsset>().unwrap().target,
            "macos-arm64"
        );
        assert_eq!(
            validate_manifest(&parsed, &endpoint, DesktopMirrorTarget::MacosX64)
                .unwrap()
                .filename,
            DesktopMirrorTarget::MacosX64.asset_filename()
        );
        manifest["assets"][2]["upstreamUrl"] =
            DesktopMirrorTarget::MacosArm64.upstream_url().into();
        let parsed: MirrorManifest = serde_json::from_value(manifest).unwrap();
        assert!(
            validate_manifest(&parsed, &endpoint, DesktopMirrorTarget::MacosX64)
                .unwrap_err()
                .to_string()
                .contains("untrusted")
        );
    }

    #[test]
    fn manifest_accepts_more_than_sixty_four_parts() {
        let endpoint = MirrorEndpoint::production();
        let payload: Vec<u8> = (0..65).collect();
        let parts: Vec<_> = payload
            .iter()
            .enumerate()
            .map(|(index, byte)| {
                let chunk = [*byte];
                let filename = format!("{ASSET_FILENAME}.part-{index:04}");
                json!({
                    "filename": filename,
                    "mirrorUrl": endpoint.part_url("future-desktop", &filename),
                    "sizeBytes": chunk.len(),
                    "sha256": digest(&chunk),
                    "encoding": "identity",
                    "decodedSizeBytes": chunk.len(),
                })
            })
            .collect();
        let manifest = json!({
            "schemaVersion": 3,
            "mirrorProvider": "cloudflare_r2",
            "upstreamRepository": "openai/codex",
            "tag": "future-desktop",
            "version": "26.930.2377.0",
            "upstreamReleaseUrl": "https://openai.com/codex/for-work/",
            "mirrorRepository": "z8hk/codex-mirror",
            "generatedAt": "2026-10-03T00:00:00Z",
            "assets": [{
                "filename": ASSET_FILENAME,
                "upstreamUrl": "https://get.microsoft.com/installer/download/9PLM9XGG6VKS",
                "sizeBytes": payload.len(),
                "sha256": digest(&payload),
                "parts": parts,
            }]
        });
        let parsed: MirrorManifest = serde_json::from_value(manifest).unwrap();

        assert!(validate_manifest(
            &parsed,
            &endpoint,
            DesktopMirrorTarget::WindowsX64
        )
        .is_ok());
    }

    #[test]
    fn msix_architecture_must_match_selected_windows_target() {
        let bytes = msix("OpenAI.Codex", "CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B");
        let package = tempfile::NamedTempFile::new().unwrap();
        fs::write(package.path(), bytes).unwrap();
        assert!(validate_msix_identity(package.path(), DesktopMirrorTarget::WindowsX64).is_ok());
        assert!(
            validate_msix_identity(package.path(), DesktopMirrorTarget::WindowsArm64)
                .unwrap_err()
                .to_string()
                .contains("architecture")
        );
    }
}
