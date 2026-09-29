//! The optional Codex Desktop host recovery entry in the Z8 Manager.
//!
//! Checking whether a host exists never starts a network request or installer.
//! The install command is a separate, explicitly confirmed operation.

use codex_plus_core::app_paths;
use codex_plus_core::codex_desktop_mirror::{
    self, DesktopMirrorEnvironment, DesktopMirrorUnsupportedReason, MirrorInstallControl,
    MirrorPhase, MirrorProgress,
};
use codex_plus_core::settings::SettingsStore;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::Emitter;

use crate::commands::CommandResult;

const HOST_INSTALL_PROGRESS_EVENT: &str = "z8-host-install-progress";
static HOST_INSTALL_IN_PROGRESS: AtomicBool = AtomicBool::new(false);
static HOST_INSTALL_CONTROL: OnceLock<Mutex<Option<Arc<MirrorInstallControl>>>> = OnceLock::new();

fn install_control_slot() -> &'static Mutex<Option<Arc<MirrorInstallControl>>> {
    HOST_INSTALL_CONTROL.get_or_init(|| Mutex::new(None))
}

struct InstallGuard;

impl InstallGuard {
    fn acquire() -> Option<Self> {
        HOST_INSTALL_IN_PROGRESS
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self)
    }
}

impl Drop for InstallGuard {
    fn drop(&mut self) {
        HOST_INSTALL_IN_PROGRESS.store(false, Ordering::Release);
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostInstallPayload {
    pub installed: bool,
    pub resumable: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HostInstallProgressPayload {
    phase: &'static str,
    completed_bytes: u64,
    total_bytes: u64,
}

fn install_result(
    status: &str,
    message: &str,
    installed: bool,
    resumable: bool,
) -> CommandResult<HostInstallPayload> {
    CommandResult {
        status: status.to_string(),
        message: message.to_string(),
        payload: HostInstallPayload {
            installed,
            resumable,
        },
    }
}

fn install_refusal(confirmed: bool, supported: bool, installed: bool) -> Option<&'static str> {
    if !confirmed {
        Some("请先在 Z8 Codex 中确认下载和安装。")
    } else if installed {
        Some("已检测到 Codex 桌面版，无需重新安装。")
    } else if !supported {
        Some("当前系统尚不支持 Codex 桌面版镜像自动安装。")
    } else {
        None
    }
}

fn progress_payload(progress: MirrorProgress) -> HostInstallProgressPayload {
    let phase = match progress.phase {
        MirrorPhase::Checking | MirrorPhase::Downloading => "download",
        MirrorPhase::Validating => "verify",
        MirrorPhase::Installing => "install",
        MirrorPhase::Complete => "complete",
    };
    HostInstallProgressPayload {
        phase,
        completed_bytes: progress.completed,
        total_bytes: progress.total,
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostStatusPayload {
    pub installed: bool,
    pub resumable: bool,
    pub supported: bool,
    pub platform: &'static str,
    pub version: Option<String>,
    pub selected_asset: Option<&'static str>,
    pub reason: Option<&'static str>,
}

fn environment_reason(reason: Option<DesktopMirrorUnsupportedReason>) -> Option<&'static str> {
    match reason {
        None => None,
        Some(DesktopMirrorUnsupportedReason::UnsupportedPlatform) => {
            Some("当前仅支持 Windows 和 macOS 安装 Codex 桌面版。")
        }
        Some(DesktopMirrorUnsupportedReason::OsVersionTooOld) => {
            Some("当前系统版本低于 Codex 桌面版镜像安装的最低预检要求。请升级系统后重试。")
        }
        Some(DesktopMirrorUnsupportedReason::ArchitectureUnknown) => {
            Some("无法确认本机芯片类型，已停止自动安装，避免下载错误版本。")
        }
    }
}

fn install_failure_message(error: &anyhow::Error) -> &'static str {
    // Do not return raw network/installer errors: they may contain local paths.
    if error
        .downcast_ref::<codex_desktop_mirror::MissingMirrorAsset>()
        .is_some()
    {
        "当前设备对应的 Z8 镜像资源尚未发布。请稍后重试，或从 Codex 官方渠道手动安装。"
    } else {
        "下载、校验或安装未完成。请检查网络连接和磁盘空间后重试。"
    }
}

fn status_for_environment(
    installed: bool,
    version: Option<String>,
    environment: DesktopMirrorEnvironment,
) -> CommandResult<HostStatusPayload> {
    let supported = environment.target.is_some();
    let platform = environment
        .target
        .map(|target| target.as_str())
        .unwrap_or(environment.platform);
    let selected_asset = environment.target.map(|target| target.asset_filename());
    let resumable = environment
        .target
        .is_some_and(codex_desktop_mirror::has_resumable_download);
    let reason = environment_reason(environment.reason);
    let message = if installed {
        "已检测到官方 Codex 桌面应用。"
    } else if supported {
        "未检测到官方 Codex 桌面应用；已匹配本机系统和芯片，安装时将检查对应镜像资源。"
    } else {
        "未检测到官方 Codex 桌面应用；当前设备未通过镜像安装环境检查。"
    };
    CommandResult {
        status: "ok".to_string(),
        message: message.to_string(),
        payload: HostStatusPayload {
            installed,
            resumable,
            supported,
            platform,
            version,
            selected_asset,
            reason,
        },
    }
}

fn installed_host_path(app_path: Option<&str>) -> Option<std::path::PathBuf> {
    let saved = SettingsStore::default()
        .load()
        .ok()
        .map(|settings| settings.codex_app_path);
    let explicit = app_path
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(std::path::Path::new);
    app_paths::resolve_codex_app_dir_with_saved(explicit, saved.as_deref())
}

#[tauri::command]
pub fn z8_host_status(app_path: Option<String>) -> CommandResult<HostStatusPayload> {
    let path = installed_host_path(app_path.as_deref());
    let installed = path.is_some();
    let version = path.as_deref().and_then(app_paths::codex_app_version);
    status_for_environment(
        installed,
        version,
        codex_desktop_mirror::inspect_desktop_mirror_environment(),
    )
}

/// An explicit UI confirmation is required even if this command is invoked
/// directly. Host detection and normal launch never call the downloader.
#[tauri::command]
pub async fn z8_install_host(
    app: tauri::AppHandle,
    confirmed: bool,
) -> CommandResult<HostInstallPayload> {
    let environment = codex_desktop_mirror::inspect_desktop_mirror_environment();
    let supported = environment.target.is_some();
    let installed = installed_host_path(None).is_some();
    if let Some(message) = install_refusal(confirmed, supported, installed) {
        return install_result(
            if confirmed && installed {
                "ok"
            } else {
                "failed"
            },
            message,
            installed,
            false,
        );
    }
    let Some(_guard) = InstallGuard::acquire() else {
        return install_result(
            "failed",
            "已有 Codex 桌面版安装正在进行，请等待完成。",
            false,
            false,
        );
    };
    // A second window may have installed the host while this request waited.
    if installed_host_path(None).is_some() {
        return install_result("ok", "已检测到 Codex 桌面版，无需重新安装。", true, false);
    }
    let control = Arc::new(MirrorInstallControl::default());
    *install_control_slot().lock().expect("install control lock poisoned") = Some(control.clone());
    let result = codex_desktop_mirror::install_for_current_platform_with_cancel(
        move |progress| {
            let _ = app.emit(HOST_INSTALL_PROGRESS_EVENT, progress_payload(progress));
        },
        &control,
    )
    .await;
    *install_control_slot().lock().expect("install control lock poisoned") = None;
    if let Err(error) = result {
        let resumable = environment
            .target
            .is_some_and(codex_desktop_mirror::has_resumable_download);
        if error
            .downcast_ref::<codex_desktop_mirror::MirrorInstallCancelled>()
            .is_some()
        {
            let message = if resumable {
                "已取消安装，已保留已下载内容，可继续安装。"
            } else {
                "已取消安装，可重新下载。"
            };
            return install_result(
                "cancelled",
                message,
                false,
                resumable,
            );
        }
        // Native errors may include local paths or network details. Keep the
        // UI message useful without exposing those values to the WebView.
        return install_result(
            "failed",
            install_failure_message(&error),
            false,
            resumable,
        );
    }
    if installed_host_path(None).is_none() {
        return install_result(
            "failed",
            "安装流程已结束，但仍未检测到 Codex 桌面版。请重新检测或手动设置应用路径。",
            false,
            false,
        );
    }
    install_result("ok", "Codex 桌面版已安装，可以启动 Z8 Codex。", true, false)
}

#[tauri::command]
pub fn z8_cancel_host_install() -> CommandResult<HostInstallPayload> {
    if !HOST_INSTALL_IN_PROGRESS.load(Ordering::Acquire) {
        return install_result("failed", "当前没有正在进行的 Codex 桌面版安装。", false, false);
    }
    let Some(control) = install_control_slot()
        .lock()
        .expect("install control lock poisoned")
        .clone()
    else {
        return install_result("failed", "安装控制尚未就绪，请稍后重试。", false, false);
    };
    if control.cancel() {
        install_result(
            "accepted",
            "正在取消安装，已下载内容会保留以便继续安装。",
            false,
            true,
        )
    } else {
        install_result(
            "failed",
            "安装已进入系统安装阶段，无法中断。",
            false,
            false,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_eligible_environment_selects_only_its_matching_asset() {
        use codex_plus_core::codex_desktop_mirror::DesktopMirrorTarget;
        for target in [
            DesktopMirrorTarget::WindowsX64,
            DesktopMirrorTarget::WindowsArm64,
            DesktopMirrorTarget::MacosX64,
            DesktopMirrorTarget::MacosArm64,
        ] {
            let status = status_for_environment(
                false,
                None,
                DesktopMirrorEnvironment {
                    platform: if target.as_str().starts_with("windows") {
                        "windows"
                    } else {
                        "macos"
                    },
                    target: Some(target),
                    reason: None,
                },
            );
            assert!(status.payload.supported);
            assert_eq!(status.payload.platform, target.as_str());
            assert_eq!(status.payload.selected_asset, Some(target.asset_filename()));
            assert_eq!(status.payload.reason, None);
        }
    }

    #[test]
    fn incompatible_environment_has_no_install_target_and_explains_os_failure() {
        let status = status_for_environment(
            false,
            None,
            DesktopMirrorEnvironment {
                platform: "macos",
                target: None,
                reason: Some(DesktopMirrorUnsupportedReason::OsVersionTooOld),
            },
        );
        assert!(!status.payload.supported);
        assert_eq!(status.payload.selected_asset, None);
        assert_eq!(
            status.payload.reason,
            Some("当前系统版本低于 Codex 桌面版镜像安装的最低预检要求。请升级系统后重试。")
        );
    }

    #[test]
    fn missing_manifest_target_is_explained_without_leaking_raw_errors() {
        let missing = anyhow::Error::new(codex_desktop_mirror::MissingMirrorAsset {
            target: "macos-arm64",
        });
        assert!(install_failure_message(&missing).contains("镜像资源尚未发布"));
        assert!(!install_failure_message(&missing).contains("macos-arm64"));

        let other = anyhow::anyhow!("private local path or mirror URL");
        assert!(!install_failure_message(&other).contains("private local path"));
    }

    #[test]
    fn install_requires_confirmation_and_a_supported_missing_host() {
        assert!(install_refusal(false, true, false).is_some());
        assert!(install_refusal(true, false, false).is_some());
        assert!(install_refusal(true, true, true).is_some());
        assert_eq!(install_refusal(true, true, false), None);
    }

    #[test]
    fn only_one_install_can_hold_the_guard() {
        let first = InstallGuard::acquire().expect("first install acquires the guard");
        assert!(InstallGuard::acquire().is_none());
        drop(first);
        assert!(InstallGuard::acquire().is_some());
    }

    #[test]
    fn progress_events_match_the_manager_ui_contract() {
        let payload = progress_payload(MirrorProgress {
            phase: MirrorPhase::Downloading,
            completed: 42,
            total: 100,
        });
        assert_eq!(
            serde_json::to_value(payload).unwrap(),
            serde_json::json!({
                "phase": "download",
                "completedBytes": 42,
                "totalBytes": 100,
            })
        );
        for (source, ui) in [
            (MirrorPhase::Checking, "download"),
            (MirrorPhase::Validating, "verify"),
            (MirrorPhase::Installing, "install"),
            (MirrorPhase::Complete, "complete"),
        ] {
            assert_eq!(
                progress_payload(MirrorProgress {
                    phase: source,
                    completed: 0,
                    total: 0
                })
                .phase,
                ui
            );
        }
    }
}
