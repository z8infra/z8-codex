//! Runtime boundary for the desktop Codex++ entry point.
//!
//! The Z8 product uses Codex++ itself as the desktop connector.  The entry
//! point must therefore not delegate startup to a separate Z8 Launch process
//! and must not download or install another Codex application as part of the
//! normal launch path.  Codex++ still needs a locally installed Codex desktop
//! application on platforms where its enhancement bridge targets that app;
//! that prerequisite is checked by [`crate::app_paths`] at launch time.

use serde::{Deserialize, Serialize};

/// The process that owns the desktop entry point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DesktopEntryOwner {
    CodexPlusPlus,
}

/// How the desktop entry obtains the Codex application it enhances.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodexAppProvisioning {
    /// Use an application that is already installed on the machine.  Download
    /// and installation are product/update operations, not runtime startup
    /// prerequisites.
    PreinstalledOnly,
}

/// The orchestration boundary for a Codex++ desktop launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopRuntimeBoundary {
    pub owner: DesktopEntryOwner,
    pub provisioning: CodexAppProvisioning,
    /// Kept explicit so a future adapter cannot silently reintroduce the
    /// independent Z8 Launch process into the desktop entry path.
    pub requires_z8_launch: bool,
}

impl DesktopRuntimeBoundary {
    /// Returns the policy used by the shipped Codex++ desktop entry.
    pub const fn codex_plus_plus() -> Self {
        Self {
            owner: DesktopEntryOwner::CodexPlusPlus,
            provisioning: CodexAppProvisioning::PreinstalledOnly,
            requires_z8_launch: false,
        }
    }

    pub const fn requires_external_codex_download(self) -> bool {
        match self.provisioning {
            CodexAppProvisioning::PreinstalledOnly => false,
        }
    }

    pub const fn requires_external_codex_install(self) -> bool {
        match self.provisioning {
            CodexAppProvisioning::PreinstalledOnly => false,
        }
    }

    pub const fn uses_codex_plus_plus_entrypoint(self) -> bool {
        matches!(self.owner, DesktopEntryOwner::CodexPlusPlus)
    }

    /// Fail closed if this policy is changed to include the independent
    /// launcher or a runtime Codex installer.  Keeping this check at the
    /// executable boundary makes the product contract testable without
    /// starting a desktop app in CI.
    pub fn validate(self) -> anyhow::Result<()> {
        if !self.uses_codex_plus_plus_entrypoint() {
            anyhow::bail!("desktop entry is not owned by Codex++");
        }
        if self.requires_z8_launch {
            anyhow::bail!("desktop entry must not require the independent Z8 Launch process");
        }
        if self.requires_external_codex_download() || self.requires_external_codex_install() {
            anyhow::bail!("Codex download/install must not be a runtime prerequisite");
        }
        Ok(())
    }
}

/// Return and validate the runtime policy used by the desktop launcher.
pub fn current() -> anyhow::Result<DesktopRuntimeBoundary> {
    let boundary = DesktopRuntimeBoundary::codex_plus_plus();
    boundary.validate()?;
    Ok(boundary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_plus_plus_desktop_entry_has_no_z8_launch_or_runtime_installer() {
        let boundary = current().expect("the shipped desktop boundary must be valid");

        assert_eq!(boundary.owner, DesktopEntryOwner::CodexPlusPlus);
        assert_eq!(boundary.provisioning, CodexAppProvisioning::PreinstalledOnly);
        assert!(!boundary.requires_z8_launch);
        assert!(!boundary.requires_external_codex_download());
        assert!(!boundary.requires_external_codex_install());
    }

    #[test]
    fn boundary_rejects_a_future_z8_launch_dependency() {
        let boundary = DesktopRuntimeBoundary {
            requires_z8_launch: true,
            ..DesktopRuntimeBoundary::codex_plus_plus()
        };

        let error = boundary.validate().expect_err("Z8 Launch must stay outside runtime");
        assert!(error.to_string().contains("Z8 Launch"));
    }

    #[test]
    fn local_codex_app_is_still_required_and_is_not_codex_plus_plus_itself() {
        use crate::launcher::{DefaultLaunchHooks, LaunchHooks};
        use crate::settings::BackendSettings;

        let directory = tempfile::tempdir().unwrap();
        let error = DefaultLaunchHooks::default()
            .resolve_app_dir(Some(directory.path()), &BackendSettings::default())
            .expect_err("an empty directory cannot supply the local desktop runtime");

        assert!(error.to_string().contains("Codex App directory not found"));
        assert!(crate::app_paths::normalize_codex_app_path(
            &directory.path().join("z8-codex.exe")
        )
        .is_none());
    }
}
