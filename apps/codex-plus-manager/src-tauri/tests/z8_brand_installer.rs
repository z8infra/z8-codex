use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("manager crate must live below the repository root")
        .to_path_buf()
}

fn read_repo(path: &str) -> String {
    fs::read_to_string(repo_root().join(path))
        .unwrap_or_else(|error| panic!("failed to read {path}: {error}"))
}

#[test]
fn branded_metadata_uses_one_product_name_and_version() {
    let tauri: serde_json::Value = serde_json::from_str(&read_repo(
        "apps/codex-plus-manager/src-tauri/tauri.conf.json",
    ))
    .expect("tauri metadata should be valid JSON");
    let package: serde_json::Value =
        serde_json::from_str(&read_repo("apps/codex-plus-manager/package.json"))
            .expect("manager package metadata should be valid JSON");
    let cargo = read_repo("Cargo.toml");

    assert_eq!(tauri["productName"], "Z8 Codex");
    let version = tauri["version"]
        .as_str()
        .expect("tauri metadata should declare a string version");
    assert!(
        version.split('.').count() == 3
            && version
                .chars()
                .all(|character| character.is_ascii_digit() || character == '.'),
        "invalid Tauri version: {version}"
    );
    assert_eq!(tauri["identifier"], "com.z8.codex.manager");
    assert_eq!(tauri["app"]["windows"][0]["title"], "Z8 Codex");
    assert_eq!(tauri["bundle"]["active"], false);
    assert_eq!(tauri["bundle"]["macOS"]["infoPlist"], "Info.plist");
    assert_eq!(
        tauri["bundle"]["windows"]["nsis"]["languages"][0],
        "SimpChinese"
    );
    assert_eq!(
        tauri["bundle"]["windows"]["nsis"]["displayLanguageSelector"],
        false
    );
    assert_eq!(package["version"].as_str(), Some(version));
    assert!(cargo.contains(&format!("version = \"{version}\"")));

    let nsi = read_repo("scripts/installer/windows/CodexPlusPlus.nsi");
    assert!(nsi.contains(&format!("!define VERSION \"{version}\"")));
    assert!(nsi.contains("Name \"Z8 Codex\""));
    assert!(nsi.contains("!insertmacro MUI_LANGUAGE \"SimpChinese\""));
    assert!(nsi.contains("CreateShortcut \"$DESKTOP\\Codex.lnk\""));
    assert!(nsi.contains("CreateShortcut \"$DESKTOP\\Z8 Codex 管理工具.lnk\""));
    assert!(nsi.contains(
        "CreateShortcut \"$DESKTOP\\Codex.lnk\" \"$INSTDIR\\z8-codex.exe\""
    ));
    assert!(nsi.contains(
        "CreateShortcut \"$DESKTOP\\Z8 Codex 管理工具.lnk\" \"$INSTDIR\\z8-codex-manager.exe\""
    ));
    assert!(nsi.contains("Publisher\" \"Z8\""));
    assert!(nsi.contains(
        "VIProductVersion \"${Z8_VERSION_MAJOR}.${Z8_VERSION_MINOR}.${Z8_VERSION_PATCH}.0\""
    ));
    assert!(nsi.contains("VIAddVersionKey /LANG=${LANG_ENGLISH} \"ProductName\" \"Z8 Codex\""));
    assert!(nsi.contains("VIAddVersionKey /LANG=${LANG_ENGLISH} \"FileVersion\" \"${VERSION}\""));

    let mac = read_repo("scripts/installer/macos/package-dmg.sh");
    assert!(mac.contains(&format!("VERSION=\"${{1:-{version}}}\"")));
    assert!(mac.contains("create_app \"Z8 Codex\""));
    assert!(mac.contains("create_app \"Z8 Codex 管理工具\""));
    assert!(mac.contains("<string>codexplusplus</string>"));

    let plist = read_repo("apps/codex-plus-manager/src-tauri/Info.plist");
    assert!(plist.contains("CFBundleURLTypes"));
    assert!(plist.contains("<string>codexplusplus</string>"));

    let user_visible_installer_text = [nsi, mac, plist].concat();
    for forbidden in [
        "JOJO",
        "jojo",
        "official Codex",
        "ChatGPT desktop",
        "BigPizzaV3",
    ] {
        assert!(
            !user_visible_installer_text.contains(forbidden),
            "installer metadata must not expose {forbidden:?}"
        );
    }
}

#[test]
fn windows_installer_preserves_coexisting_codexplusplus_installations() {
    let nsi = read_repo("scripts/installer/windows/CodexPlusPlus.nsi");

    // Installation and removal run in the current user's profile. This is required
    // for a per-user installer and prevents another user's shortcuts being touched.
    assert!(nsi.contains("RequestExecutionLevel user"));
    let (declarations, install_and_uninstall) = nsi
        .split_once("Section \"Install\"")
        .expect("NSIS install section");
    let (install, uninstall_section) = install_and_uninstall
        .split_once("SectionEnd")
        .expect("NSIS install section end");
    let uninstall = uninstall_section
        .split_once("Section \"Uninstall\"")
        .expect("NSIS uninstall section")
        .1;
    assert!(!declarations.contains("SetShellVarContext"));
    assert!(install.contains("SetShellVarContext current"));
    assert!(uninstall.contains("SetShellVarContext current"));

    // The upstream product can remain installed beside Z8. Neither section may
    // remove its shortcuts, registry/config data, or kill a process with a shared
    // technical executable name.
    for legacy_shortcut in [
        "$DESKTOP\\Codex++.lnk",
        "$DESKTOP\\Codex++ 管理工具.lnk",
        "$SMPROGRAMS\\Codex++\\Codex++.lnk",
        "$SMPROGRAMS\\Codex++\\Codex++ 管理工具.lnk",
    ] {
        assert!(
            !install.contains(legacy_shortcut) && !uninstall.contains(legacy_shortcut),
            "installer must preserve an upstream shortcut: {legacy_shortcut}"
        );
    }
    assert!(!install.contains("taskkill /IM codex-plus-plus"));
    assert!(!uninstall.contains("taskkill /IM codex-plus-plus"));
    assert!(!install.contains("$SMPROGRAMS\\Codex++"));
    assert!(!uninstall.contains("$SMPROGRAMS\\Codex++"));
    assert!(!nsi.contains("DeleteRegKey HKCU \"Software\\Codex++\""));
    assert!(!nsi.contains(
        "DeleteRegKey HKCU \"Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Codex++\""
    ));
    assert!(!nsi.contains("RMDir /r \"$APPDATA"));
    assert!(!nsi.contains("RMDir /r \"$LOCALAPPDATA"));

    // The only recursive directory operation is the installer-owned Start Menu
    // folder, and it is guarded by the exact Z8 folder name.
    assert!(nsi.contains("RMDir \"$SMPROGRAMS\\Z8 Codex\""));
    assert!(nsi.contains("DeleteRegKey HKCU \"Software\\Z8 Codex\""));
}

#[test]
fn windows_installer_fails_closed_when_z8_executables_cannot_be_replaced_or_removed() {
    let nsi = read_repo("scripts/installer/windows/CodexPlusPlus.nsi").replace("\r\n", "\n");
    let (_, sections) = nsi
        .split_once("Section \"Install\"")
        .expect("install section");
    let (install, sections) = sections.split_once("SectionEnd").expect("install end");
    let (_, uninstall) = sections
        .split_once("Section \"Uninstall\"")
        .expect("uninstall section");

    // NSIS must not let a user ignore an extraction error and report a
    // successful upgrade with only one of the two binaries replaced.
    assert!(nsi.contains("AllowSkipFiles off"));
    assert!(!nsi.contains("AllowSkipFiles on"));
    assert!(!nsi.contains("SetOverwrite off"));
    assert!(install.contains("SetErrorLevel 2\n  Abort"));
    assert!(uninstall.contains("SetErrorLevel 2\n  Abort"));

    let first_install_file = install
        .find("File \"${ROOT}\\dist\\windows\\app\\z8-codex.exe\"")
        .expect("launcher extraction");
    let first_uninstall_shortcut = uninstall
        .find("Delete \"$DESKTOP\\Codex.lnk\"")
        .expect("Z8 shortcut cleanup");

    for executable in [
        "z8-codex.exe",
        "z8-codex-manager.exe",
        "codex-plus-plus.exe",
        "codex-plus-plus-manager.exe",
    ] {
        let probe = format!("FileOpen $0 \"$INSTDIR\\{executable}\" a");
        let install_probe = install.find(&probe).expect("Z8 upgrade write preflight");
        let uninstall_probe = uninstall
            .find(&probe)
            .expect("Z8 uninstall write preflight");
        assert!(install.contains(&format!("{probe}\n  IfErrors z8_install_locked")));
        assert!(uninstall.contains(&format!("{probe}\n  IfErrors z8_uninstall_locked")));
        assert!(install_probe < first_install_file);

        if executable.starts_with("z8-") {
            let extraction = format!("File \"${{ROOT}}\\dist\\windows\\app\\{executable}\"");
            let extraction_check = format!("{extraction}\n  IfErrors z8_install_locked");
            assert!(install.contains(&extraction_check));
        }

        let deletion = format!("Delete \"$INSTDIR\\{executable}\"");
        let deletion_check = format!("{deletion}\n  IfErrors z8_uninstall_locked");
        assert!(uninstall.contains(&deletion_check));
        assert!(uninstall.contains(&format!(
            "IfFileExists \"$INSTDIR\\{executable}\" 0 z8_uninstall_"
        )));
        assert!(uninstall_probe < uninstall.find(&deletion).unwrap());
        assert!(uninstall.find(&deletion).unwrap() < first_uninstall_shortcut);
    }

    // Neither section can use a shared executable name to terminate a
    // separately installed Codex++ instance.
    assert!(!install.contains("taskkill"));
    assert!(!uninstall.contains("taskkill"));
    assert!(!uninstall.contains("Delete /REBOOTOK"));
}

#[test]
fn windows_update_mode_closes_z8_processes_before_install_preflight() {
    let nsi = read_repo("scripts/installer/windows/CodexPlusPlus.nsi").replace("\r\n", "\n");
    let (init, after_init) = nsi
        .split_once("Function z8_update_close_processes")
        .expect("update process helper");
    let (helper, sections) = after_init
        .split_once("Section \"Install\"")
        .expect("install section");
    let (install, sections) = sections.split_once("SectionEnd").expect("install end");
    let uninstall = sections
        .split_once("Section \"Uninstall\"")
        .expect("uninstall section")
        .1;

    assert!(init.contains("Var Z8_UPDATE_MODE"));
    assert!(init.contains("${GetOptions} \"$R0\" \"/Z8Update\" $R1"));
    assert!(helper.contains("taskkill.exe"));
    assert!(helper.contains("/IM z8-codex-manager.exe"));
    assert!(helper.contains("/IM z8-codex.exe"));
    assert!(install.contains("${If} $Z8_UPDATE_MODE == \"1\""));
    assert!(install.contains("Call z8_update_close_processes"));
    assert!(install.find("Call z8_update_close_processes").unwrap()
        < install.find("FileOpen $0 \"$INSTDIR\\z8-codex.exe\" a").unwrap());
    assert!(!uninstall.contains("z8_update_close_processes"));
}

#[test]
fn windows_release_and_pr_build_package_native_x64_and_arm64_wrappers() {
    let release = read_repo(".github/workflows/release-assets.yml");
    for required in [
        "runner: windows-latest",
        "target: x86_64-pc-windows-msvc",
        "cargo build --release --locked --target ${{ matrix.target }}",
        "target/${{ matrix.target }}/release/codex-plus-plus.exe",
        "target/${{ matrix.target }}/release/codex-plus-plus-manager.exe",
        "dist/windows/app/z8-codex.exe",
        "dist/windows/app/z8-codex-manager.exe",
        "verify-pe-machine.ps1",
        "/DARCH=${{ matrix.arch }}",
    ] {
        assert!(release.contains(required), "release workflow must contain {required:?}");
    }
    assert!(!release.contains("runner: windows-11-arm"));
    assert!(!release.contains("target: aarch64-pc-windows-msvc"));

    let pr = read_repo(".github/workflows/pr-build.yml");
    for required in [
        "runner: windows-latest",
        "target: x86_64-pc-windows-msvc",
        "runner: windows-11-arm",
        "target: aarch64-pc-windows-msvc",
        "cargo build --release --locked --target ${{ matrix.target }}",
        "target/${{ matrix.target }}/release/codex-plus-plus.exe",
        "target/${{ matrix.target }}/release/codex-plus-plus-manager.exe",
        "dist/windows/app/z8-codex.exe",
        "dist/windows/app/z8-codex-manager.exe",
        "verify-pe-machine.ps1",
        "/DARCH=${{ matrix.arch }}",
    ] {
        assert!(pr.contains(required), "PR workflow must contain {required:?}");
    }

    assert!(release.contains("Z8Codex-$version-windows-${{ matrix.arch }}.zip"));
    assert!(release.contains("dist/windows/*.exe"));
    assert!(release.contains("- windows-installer"));

    assert!(pr.contains("Z8Codex-windows-${{ matrix.arch }}-installer"));
    assert!(pr.contains("Z8Codex-windows-${{ matrix.arch }}-binaries"));

    let nsi = read_repo("scripts/installer/windows/CodexPlusPlus.nsi");
    assert!(nsi.contains("Z8Codex-${VERSION}-windows-${ARCH}-setup.exe"));
    assert!(nsi.contains("${IsNativeAMD64}"));
    assert!(nsi.contains("${IsNativeARM64}"));
    assert!(nsi.contains("ARCH must be x64 or arm64"));
}

#[test]
fn macos_workflows_verify_the_branded_bundles_created_by_the_dmg_script() {
    let script = read_repo("scripts/installer/macos/package-dmg.sh");
    let release = read_repo(".github/workflows/release-assets.yml");
    let pr = read_repo(".github/workflows/pr-build.yml");

    for app_name in ["Z8 Codex", "Z8 Codex 管理工具"] {
        assert!(script.contains(&format!("create_app \"{app_name}\"")));
        let staged = format!("dist/macos/stage/{app_name}.app");
        assert!(release.contains(&staged), "release must verify {staged}");
        assert!(pr.contains(&staged), "PR must verify {staged}");
    }
    let version_preflight = script
        .find("validate_bundle_version\n")
        .expect("the DMG script must validate Info.plist versions");
    let arch_preflight = script
        .find("verify_input_binaries\n")
        .expect("the DMG script must check both native binaries");
    let replace_dist = script
        .find("rm -rf \"$DIST\"")
        .expect("the DMG script replaces its own output directory");
    assert!(version_preflight < arch_preflight && arch_preflight < replace_dist);
    assert!(script.contains("if [ \"$ARCH\" = \"x86_64\" ]; then\n  ARCH=\"x64\""));
    assert!(script.contains("actual_arch=\"$(lipo -archs \"$binary_path\")\""));
    assert!(script.contains("<string>$BUNDLE_BUILD_VERSION</string>"));
    assert!(script.contains("<string>$BUNDLE_SHORT_VERSION</string>"));
    assert!(script.contains("codesign --verify --deep --strict \"$app_dir\""));
    for workflow in [&release, &pr] {
        assert!(workflow.contains("MACOS_BUILD_NUMBER=\"$GITHUB_RUN_NUMBER\""));
        assert!(workflow.contains("test \"$(lipo -archs \"$executable\")\" = \"$expected_arch\""));
        assert!(workflow.contains("codesign --verify --deep --strict \"$app\""));
        assert!(!workflow.contains("codesign -dv \"$app\""));
    }
    for stale in [
        "dist/macos/stage/Codex++.app",
        "dist/macos/stage/Codex++ 管理工具.app",
    ] {
        assert!(!release.contains(stale));
        assert!(!pr.contains(stale));
    }
    assert!(release.contains("dist/macos/*.dmg"));
    assert!(release.contains("Build macOS zip asset"));
    assert!(release.contains("dist/macos/*.zip"));
    assert!(pr.contains("Z8Codex-macos-${{ matrix.arch }}-dmg"));
    assert!(release.contains("Copy-Item LICENSE dist/windows/app/"));
    let windows_job = release
        .split("  windows-installer:")
        .nth(1)
        .expect("Windows release job")
        .split("  macos-dmg:")
        .next()
        .expect("Windows release job end");
    let macos_job = release
        .split("  macos-dmg:")
        .nth(1)
        .expect("macOS release job")
        .split("  latest-json:")
        .next()
        .expect("macOS release job end");
    assert!(windows_job.contains("needs: prepare-release"));
    assert!(macos_job.contains("needs: prepare-release"));
}

#[test]
fn release_assets_stay_draft_until_all_platform_assets_are_ready() {
    let release = read_repo(".github/workflows/release-assets.yml");
    assert!(release.contains("--draft"));
    assert!(release.contains("Verify release is draft before asset uploads"));
    assert!(release.contains("isDraft"));
    assert!(release.contains("gh release upload \"$TAG\" latest.json --clobber"));
    assert!(release.contains("gh release edit \"$TAG\" --draft=false --latest"));
    assert!(release.contains("- windows-installer"));
    assert!(release.contains("- macos-dmg"));
}

#[test]
fn pr_build_does_not_duplicate_the_full_matrix_on_main_pushes() {
    let pr = read_repo(".github/workflows/pr-build.yml");
    assert!(pr.contains("  pull_request:"));
    assert!(pr.contains("  workflow_dispatch:"));
    assert!(!pr.contains("  push:\n    branches: [main]"));
}

#[test]
fn linux_package_metadata_is_z8_branded_and_declares_host_boundary() {
    let script = read_repo("scripts/installer/linux/build-deb.sh");
    assert!(script.contains("Name=Z8 Codex"));
    assert!(script.contains("Name=Z8 Codex 管理工具"));
    assert!(script.contains("Maintainer: Z8 Codex Team <support@z8.hk>"));
    assert!(!script.contains("BigPizzaV3"));
    assert!(!script.contains("1727732@qq.com"));
    assert!(script.contains("preinstalled Codex desktop app"));
    assert!(script.contains("does not download or install a runtime during startup"));
    // The package must not ask apt to install a package named `codex`: the
    // official desktop host is an external prerequisite, not a Debian
    // dependency that this installer can fetch or replace.
    assert!(!script.contains("Recommends: codex"));
    assert!(script.contains("Installed-Size: ${installed_size}"));
    assert!(!script.contains("Installed-Size: 70620"));
}
