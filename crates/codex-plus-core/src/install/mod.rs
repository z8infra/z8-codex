use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

pub mod macos;
pub mod windows;

pub const SILENT_NAME: &str = "Z8 Codex";
pub const MANAGER_NAME: &str = "Z8 Codex 管理工具";
pub const WINDOWS_SILENT_SHORTCUT_NAME: &str = "Codex";
pub const SILENT_BINARY: &str = "codex-plus-plus";
pub const WINDOWS_SILENT_BINARY: &str = "z8-codex";
pub const MACOS_SILENT_EXECUTABLE: &str = "CodexPlusPlus";
pub const MANAGER_BINARY: &str = "codex-plus-plus-manager";
pub const WINDOWS_MANAGER_BINARY: &str = "z8-codex-manager";
pub const SILENT_BUNDLE_ID: &str = "com.z8.codex";
pub const MANAGER_BUNDLE_ID: &str = "com.z8.codex.manager";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct InstallOptions {
    #[serde(default)]
    pub install_root: Option<PathBuf>,
    #[serde(default)]
    pub launcher_path: Option<PathBuf>,
    #[serde(default)]
    pub manager_path: Option<PathBuf>,
    #[serde(default)]
    pub remove_owned_data: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShortcutState {
    pub installed: bool,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EntryPointState {
    pub silent_shortcut: ShortcutState,
    pub management_shortcut: ShortcutState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallActionResult {
    pub status: String,
    pub message: String,
    pub silent_shortcut: ShortcutState,
    pub management_shortcut: ShortcutState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacosAppBundle {
    pub app_path: PathBuf,
    pub info_plist: String,
    pub launch_script: String,
    pub binary_source: Option<PathBuf>,
    pub binary_target_name: Option<String>,
}

impl ShortcutState {
    pub fn missing(path: Option<PathBuf>) -> Self {
        Self {
            installed: false,
            path: path.map(|path| path.to_string_lossy().to_string()),
        }
    }

    pub fn from_candidates(candidates: Vec<PathBuf>) -> Self {
        if let Some(path) = candidates.iter().find(|path| path.exists()) {
            return Self {
                installed: true,
                path: Some(path.to_string_lossy().to_string()),
            };
        }
        Self::missing(candidates.into_iter().next())
    }
}

pub fn shortcut_names() -> (&'static str, &'static str) {
    ("Codex.lnk", "Z8 Codex 管理工具.lnk")
}

pub fn app_bundle_names() -> (&'static str, &'static str) {
    ("Z8 Codex.app", "Z8 Codex 管理工具.app")
}

pub fn inspect_entrypoints() -> EntryPointState {
    let root = default_install_root();
    EntryPointState {
        silent_shortcut: ShortcutState::from_candidates(entrypoint_candidates(&root, false)),
        management_shortcut: ShortcutState::from_candidates(entrypoint_candidates(&root, true)),
    }
}

pub fn install_entrypoints(options: &InstallOptions) -> InstallActionResult {
    let result = platform_install(options);
    action_result(result, "入口已安装。")
}

pub fn uninstall_entrypoints(options: &InstallOptions) -> InstallActionResult {
    let result = platform_uninstall(options);
    if result.is_ok() && options.remove_owned_data {
        let _ = remove_owned_data();
    }
    action_result(result, "入口已卸载。")
}

pub fn repair_entrypoints(options: &InstallOptions) -> InstallActionResult {
    let result = platform_install(options);
    action_result(result, "入口已修复。")
}

pub fn build_windows_entrypoint_plan(options: &InstallOptions) -> windows::WindowsEntrypointPlan {
    windows::build_windows_entrypoint_plan(options)
}

pub fn build_macos_app_bundle(options: &InstallOptions, manager: bool) -> MacosAppBundle {
    macos::build_app_bundle(options, manager)
}

pub fn remove_owned_data() -> std::io::Result<()> {
    let dir = crate::paths::default_app_state_dir();
    if !dir.exists() {
        return Ok(());
    }
    // 卸载流程会递归删除，路径来自环境/推导，先过一道"不许删 CODEX_HOME 及其祖先"
    // 的兜底（#2146）。守卫只在这条路径确实指向 home 时才会拒绝，正常卸载不受影响。
    if let Err(error) = crate::codex_home::ensure_safe_recursive_removal(
        &dir,
        &crate::codex_home::default_codex_home_dir(),
    ) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            error.to_string(),
        ));
    }
    std::fs::remove_dir_all(dir)?;
    Ok(())
}

pub fn default_install_root() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        return crate::windows_integration::desktop_dir().or_else(|| {
            directories::UserDirs::new().and_then(|dirs| dirs.desktop_dir().map(PathBuf::from))
        });
    }

    #[cfg(target_os = "macos")]
    {
        let sys_apps = PathBuf::from("/Applications");
        if sys_apps.join(format!("{SILENT_NAME}.app")).exists()
            || sys_apps.join(format!("{MANAGER_NAME}.app")).exists()
        {
            return Some(sys_apps);
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = macos_applications_dir_from_exe(&exe) {
                if is_macos_applications_dir(&dir) {
                    return Some(dir);
                }
            }
        }
        return Some(sys_apps);
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        directories::UserDirs::new().and_then(|dirs| dirs.desktop_dir().map(PathBuf::from))
    }
}

pub fn default_install_root_strategy() -> &'static str {
    if cfg!(windows) {
        "windows-known-folder"
    } else if cfg!(target_os = "macos") {
        "macos-applications"
    } else {
        "user-dirs-desktop"
    }
}

fn platform_install(options: &InstallOptions) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        windows::install_shortcuts(options)
    }

    #[cfg(target_os = "macos")]
    {
        macos::install_app_bundles(options)
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = options;
        anyhow::bail!("当前平台暂不支持安装 Z8 Codex 入口")
    }
}

fn platform_uninstall(options: &InstallOptions) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        windows::uninstall_shortcuts(options)
    }

    #[cfg(target_os = "macos")]
    {
        macos::uninstall_app_bundles(options)
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = options;
        anyhow::bail!("当前平台暂不支持卸载 Z8 Codex 入口")
    }
}

fn action_result(result: anyhow::Result<()>, success_message: &str) -> InstallActionResult {
    let state = inspect_entrypoints();
    match result {
        Ok(()) => InstallActionResult {
            status: "ok".to_string(),
            message: success_message.to_string(),
            silent_shortcut: state.silent_shortcut,
            management_shortcut: state.management_shortcut,
        },
        Err(error) => InstallActionResult {
            status: "failed".to_string(),
            message: error.to_string(),
            silent_shortcut: state.silent_shortcut,
            management_shortcut: state.management_shortcut,
        },
    }
}

fn entrypoint_candidates(root: &Option<PathBuf>, manager: bool) -> Vec<PathBuf> {
    let Some(root) = root else {
        return Vec::new();
    };
    let name = if manager { MANAGER_NAME } else { SILENT_NAME };
    if cfg!(windows) {
        let legacy = if manager {
            "Codex++ 管理工具.lnk"
        } else {
            "Z8 Codex.lnk"
        };
        if manager {
            vec![root.join(format!("{name}.lnk")), root.join(legacy)]
        } else {
            vec![
                root.join(format!("{WINDOWS_SILENT_SHORTCUT_NAME}.lnk")),
                root.join(legacy),
                root.join("Codex++.lnk"),
            ]
        }
    } else if cfg!(target_os = "macos") {
        let legacy = if manager {
            "Codex++ 管理工具.app"
        } else {
            "Codex++.app"
        };
        vec![root.join(format!("{name}.app")), root.join(legacy)]
    } else {
        let legacy = if manager {
            "Codex++ 管理工具.desktop"
        } else {
            "Codex++.desktop"
        };
        vec![root.join(format!("{name}.desktop")), root.join(legacy)]
    }
}

pub fn option_or_current_exe(value: &Option<PathBuf>, binary: &str) -> PathBuf {
    if let Some(value) = value {
        return value.clone();
    }
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("."));
    companion_binary_path_from_exe(&exe, binary)
}

pub fn companion_binary_path(binary: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("."));
    companion_binary_path_from_exe(&exe, native_binary_name(binary))
}

pub fn spawn_companion<I, S>(binary: &str, args: I) -> anyhow::Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let binary = native_binary_name(binary);
    let args = args
        .into_iter()
        .map(|arg| arg.as_ref().to_os_string())
        .collect::<Vec<OsString>>();

    #[cfg(target_os = "macos")]
    {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("."));
        if let Some(bundle_id) = macos_companion_bundle_identifier_from_exe(&exe, binary) {
            let launch_result = Command::new("/usr/bin/open")
                .args(["-n", "-b", bundle_id, "--args"])
                .args(&args)
                .status();
            if launch_result.as_ref().is_ok_and(|status| status.success()) {
                return Ok(format!("bundle:{bundle_id}"));
            }
            let fallback = companion_binary_path_from_exe(&exe, binary);
            if !fallback.exists() {
                let detail = launch_result
                    .map(|status| status.to_string())
                    .unwrap_or_else(|error| error.to_string());
                anyhow::bail!("macOS Launch Services 无法启动 bundle {bundle_id}：{detail}");
            }
        }
    }

    let path = companion_binary_path(binary);
    let mut command = Command::new(&path);
    command.args(&args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(crate::windows_create_no_window());
    }
    command
        .spawn()
        .map_err(|error| anyhow::anyhow!("无法启动 {}：{error}", path.to_string_lossy()))?;
    Ok(path.to_string_lossy().to_string())
}

pub fn open_or_activate_manager() -> anyhow::Result<String> {
    #[cfg(target_os = "macos")]
    {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("."));
        if let Some(bundle_id) = macos_companion_bundle_identifier_from_exe(&exe, MANAGER_BINARY) {
            let activated = Command::new("/usr/bin/open")
                .args(["-b", bundle_id])
                .status()
                .is_ok_and(|status| status.success());
            if activated {
                return Ok(format!("bundle:{bundle_id}"));
            }
        }
    }

    spawn_companion(MANAGER_BINARY, std::iter::empty::<&str>())
}

fn native_binary_name(binary: &str) -> &str {
    #[cfg(windows)]
    {
        return match binary {
            SILENT_BINARY | WINDOWS_SILENT_BINARY => WINDOWS_SILENT_BINARY,
            MANAGER_BINARY | WINDOWS_MANAGER_BINARY => WINDOWS_MANAGER_BINARY,
            _ => binary,
        };
    }

    #[cfg(not(windows))]
    {
        binary
    }
}

pub fn macos_companion_bundle_identifier_from_exe(
    exe: &Path,
    binary: &str,
) -> Option<&'static str> {
    let (_, app_name) = macos_applications_dir_and_app_name_from_exe(exe)?;
    let known_bundle =
        app_name == format!("{SILENT_NAME}.app") || app_name == format!("{MANAGER_NAME}.app");
    if !known_bundle {
        return None;
    }
    match binary {
        SILENT_BINARY => Some(SILENT_BUNDLE_ID),
        MANAGER_BINARY => Some(MANAGER_BUNDLE_ID),
        _ => None,
    }
}

pub fn companion_binary_path_from_exe(exe: &Path, binary: &str) -> PathBuf {
    let dir = exe.parent().unwrap_or_else(|| Path::new("."));
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    if let Some(bundle_binary) = macos_companion_binary_from_exe(exe, binary) {
        // A local Tauri bundle contains the manager only. Prefer the freshly
        // built launcher beside `target/release` when the sibling app is not
        // present, while keeping the installed /Applications layout intact.
        if bundle_binary.exists() || !is_macos_development_bundle(exe) {
            return bundle_binary;
        }
    }
    #[cfg(target_os = "macos")]
    if let Some(development_binary) = macos_development_companion_binary(exe, binary) {
        return development_binary;
    }
    let same_bundle = dir.join(binary);
    if same_bundle.exists() {
        return same_bundle;
    }
    let expected = dir.join(format!("{binary}{suffix}"));
    if expected.exists() {
        return expected;
    }

    // Tauri bundles keep `bundle.resources` below a sibling `resources`
    // directory instead of placing files beside the application executable.
    // Keep the normal sibling layout as the first choice (the custom NSIS
    // package uses it), then accept the Tauri resource layout as a fallback so
    // a manager-only bundle can still launch its packaged companion.
    let resource_dir = dir.join("resources");
    let resource_binary = resource_dir.join(format!("{binary}{suffix}"));
    if resource_binary.exists() {
        return resource_binary;
    }
    #[cfg(windows)]
    if let Some(legacy_binary) = legacy_windows_binary_name(binary) {
        let legacy = dir.join(format!("{legacy_binary}.exe"));
        if legacy.exists() {
            return legacy;
        }
        let resource_legacy = resource_dir.join(format!("{legacy_binary}.exe"));
        if resource_legacy.exists() {
            return resource_legacy;
        }
    }
    expected
}

#[cfg(windows)]
fn legacy_windows_binary_name(binary: &str) -> Option<&'static str> {
    match binary {
        "z8-codex" => Some("codex-plus-plus"),
        "z8-codex-manager" => Some("codex-plus-plus-manager"),
        _ => None,
    }
}

fn is_macos_development_bundle(exe: &Path) -> bool {
    exe.components()
        .any(|component| component.as_os_str() == "target")
        && exe
            .components()
            .any(|component| component.as_os_str() == "bundle")
}

#[cfg(target_os = "macos")]
fn macos_development_companion_binary(exe: &Path, binary: &str) -> Option<PathBuf> {
    let mut path = exe.parent()?;
    while let Some(parent) = path.parent() {
        if matches!(
            path.file_name().and_then(|name| name.to_str()),
            Some("release" | "debug")
        ) {
            let candidate = path.join(binary);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        path = parent;
    }
    None
}

fn macos_companion_binary_from_exe(exe: &Path, binary: &str) -> Option<PathBuf> {
    let (applications_dir, app_name) = macos_applications_dir_and_app_name_from_exe(exe)?;
    if binary == SILENT_BINARY {
        if app_name == format!("{SILENT_NAME}.app") {
            return Some(macos_preferred_bundle_binary(
                exe,
                SILENT_BINARY,
                "CodexPlusPlus",
            ));
        }
        let macos = applications_dir
            .join(format!("{SILENT_NAME}.app"))
            .join("Contents")
            .join("MacOS");
        return Some(
            macos
                .join(SILENT_BINARY)
                .exists()
                .then(|| macos.join(SILENT_BINARY))
                .unwrap_or_else(|| macos.join("CodexPlusPlus")),
        );
    }
    if binary == MANAGER_BINARY {
        if app_name == format!("{MANAGER_NAME}.app") {
            return Some(macos_preferred_bundle_binary(
                exe,
                MANAGER_BINARY,
                "CodexPlusPlusManager",
            ));
        }
        let macos = applications_dir
            .join(format!("{MANAGER_NAME}.app"))
            .join("Contents")
            .join("MacOS");
        return Some(
            macos
                .join(MANAGER_BINARY)
                .exists()
                .then(|| macos.join(MANAGER_BINARY))
                .unwrap_or_else(|| macos.join("CodexPlusPlusManager")),
        );
    }
    None
}

fn macos_preferred_bundle_binary(
    exe: &Path,
    sidecar_name: &str,
    bundle_executable_name: &str,
) -> PathBuf {
    let macos = exe.parent().unwrap_or_else(|| Path::new("."));
    let sidecar = macos.join(sidecar_name);
    if sidecar.exists() {
        return sidecar;
    }
    let bundle_executable = macos.join(bundle_executable_name);
    if bundle_executable.exists() {
        return bundle_executable;
    }
    exe.to_path_buf()
}

#[cfg(target_os = "macos")]
fn macos_applications_dir_from_exe(exe: &Path) -> Option<PathBuf> {
    macos_applications_dir_and_app_name_from_exe(exe).map(|(dir, _)| dir)
}

fn macos_applications_dir_and_app_name_from_exe(exe: &Path) -> Option<(PathBuf, String)> {
    let mut path = exe;
    while let Some(parent) = path.parent() {
        if path.extension().and_then(|extension| extension.to_str()) == Some("app") {
            let app_name = path.file_name()?.to_string_lossy().to_string();
            return Some((parent.to_path_buf(), app_name));
        }
        path = parent;
    }
    None
}

#[cfg(target_os = "macos")]
fn is_macos_applications_dir(path: &Path) -> bool {
    if path == Path::new("/Applications") {
        return true;
    }
    directories::BaseDirs::new()
        .map(|dirs| path == dirs.home_dir().join("Applications"))
        .unwrap_or(false)
}

pub(crate) fn install_root_or_default(options: &InstallOptions) -> PathBuf {
    options
        .install_root
        .clone()
        .or_else(default_install_root)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(all(test, windows))]
mod tests {
    use super::{
        MANAGER_BINARY, WINDOWS_MANAGER_BINARY, WINDOWS_SILENT_BINARY, SILENT_BINARY,
        ShortcutState, companion_binary_path_from_exe, entrypoint_candidates, native_binary_name,
    };

    #[test]
    fn legacy_companion_requests_map_to_renamed_windows_binaries() {
        assert_eq!(native_binary_name(SILENT_BINARY), WINDOWS_SILENT_BINARY);
        assert_eq!(native_binary_name(MANAGER_BINARY), WINDOWS_MANAGER_BINARY);
    }

    #[test]
    fn installed_codex_shortcut_is_recognized_before_legacy_names() {
        let root = tempfile::tempdir().unwrap();
        let codex_shortcut = root.path().join("Codex.lnk");
        std::fs::write(&codex_shortcut, b"shortcut").unwrap();

        let candidates = entrypoint_candidates(&Some(root.path().to_path_buf()), false);
        let state = ShortcutState::from_candidates(candidates);

        assert!(state.installed);
        assert_eq!(state.path, Some(codex_shortcut.to_string_lossy().to_string()));
    }

    #[test]
    fn legacy_z8_codex_shortcut_remains_recognized() {
        let root = tempfile::tempdir().unwrap();
        let legacy_shortcut = root.path().join("Z8 Codex.lnk");
        std::fs::write(&legacy_shortcut, b"legacy shortcut").unwrap();

        let state = ShortcutState::from_candidates(entrypoint_candidates(
            &Some(root.path().to_path_buf()),
            false,
        ));

        assert!(state.installed);
        assert_eq!(
            state.path,
            Some(legacy_shortcut.to_string_lossy().to_string())
        );
    }

    #[test]
    fn tauri_resource_companion_is_used_when_sibling_binary_is_missing() {
        let root = tempfile::tempdir().unwrap();
        let manager = root.path().join("z8-codex-manager.exe");
        let resource_dir = root.path().join("resources");
        std::fs::create_dir_all(&resource_dir).unwrap();
        let companion = resource_dir.join("z8-codex.exe");
        std::fs::write(&companion, b"launcher").unwrap();

        assert_eq!(
            companion_binary_path_from_exe(&manager, WINDOWS_SILENT_BINARY),
            companion
        );
    }
}
