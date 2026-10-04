//! Read-only compatibility notice for data left by retired integrations.
//!
//! Historical Dream Skin backups do not record the values last applied to the
//! host. They cannot prove ownership of its current appearance, so neither the
//! host configuration nor the backup is modified automatically.

use std::path::Path;

pub const LEGACY_THEME_BACKUP_FILE: &str = "dream-skin-base-theme-backup.json";

pub fn legacy_theme_backup_notice(state_dir: &Path) -> Option<&'static str> {
    match std::fs::symlink_metadata(state_dir.join(LEGACY_THEME_BACKUP_FILE)) {
        Ok(_) => Some(
            "检测到旧 Dream Skin 外观备份。皮肤功能已停用；备份缺少外观归属证明，已保留宿主设置和原备份，未自动恢复。",
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => Some("无法检查旧外观备份；皮肤功能已停用，未修改宿主设置或备份。"),
    }
}
