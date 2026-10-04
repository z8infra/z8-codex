use codex_plus_core::retired_features::{LEGACY_THEME_BACKUP_FILE, legacy_theme_backup_notice};
use codex_plus_core::settings::{BackendSettings, SettingsStore};
use codex_plus_core::tools::{ToolId, tool_specs};
use serde_json::{Value, json};

#[test]
fn legacy_hidden_settings_stay_inert_and_survive_load_update_save() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("settings.json");
    let original = json!({
        "codexAppDreamSkinEnabled": true,
        "codexAppDreamSkinPaused": false,
        "codexAppDreamSkinThemeConfig": {"id": "custom", "extra": [1, 2]},
        "codexAppDreamSkinImagePath": "user-owned.png",
        "codexAppZedRemoteOpen": true,
        "zedRemoteOpenStrategy": "newWindow",
        "zedRemoteProjectRegistryEnabled": true,
        "zedRemoteSyncToZedSettings": true,
        "activeTool": "grok",
        "tools": {
            "grok": {"activeRelayId": "grok-a", "unknownFutureField": {"kept": true}},
            "future": {"custom": ["preserved"]}
        },
        "unrelatedFutureSettings": {"nested": [1, "two"]},
        "codexAppImageOverlayEnabled": true,
        "codexGoalsEnabled": false,
        "relayContextConfigContents": "[mcp_servers.local]\ncommand = \"local-tool\"\n"
    });
    std::fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
    let store = SettingsStore::new(path.clone());

    for _ in 0..2 {
        let loaded = store.load().unwrap();
        assert_eq!(loaded.active_tool, ToolId::Codex);
        let effective = serde_json::to_value(&loaded).unwrap();
        for key in ["codexAppDreamSkinEnabled", "codexAppZedRemoteOpen", "zedRemoteProjectRegistryEnabled"] {
            assert!(effective.get(key).is_none(), "retired switch {key} is not active configuration");
        }
        let updated = store.update(json!({
            "codexAppDreamSkinEnabled": true,
            "codexAppZedRemoteOpen": true,
            "activeTool": "grok",
            "codexExtraArgs": ["--log-level=debug"]
        })).unwrap();
        assert_eq!(updated.active_tool, ToolId::Codex);
        store.save(&updated).unwrap();
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for key in ["codexAppDreamSkinEnabled", "codexAppDreamSkinPaused", "codexAppDreamSkinThemeConfig", "codexAppDreamSkinImagePath", "codexAppZedRemoteOpen", "zedRemoteOpenStrategy", "zedRemoteProjectRegistryEnabled", "zedRemoteSyncToZedSettings", "unrelatedFutureSettings"] {
            assert_eq!(saved[key], original[key], "inactive data {key} must survive");
        }
        assert_eq!(saved["tools"]["grok"], original["tools"]["grok"]);
        assert_eq!(saved["tools"]["future"], original["tools"]["future"]);
        assert_eq!(saved["activeTool"], "codex");
        assert_eq!(saved["codexAppImageOverlayEnabled"], true);
        assert_eq!(saved["codexGoalsEnabled"], false);
        assert!(saved["relayContextConfigContents"].as_str().unwrap().contains("mcp_servers.local"));
    }
    let specs = tool_specs();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].id, ToolId::Codex);
    assert!(!codex_plus_core::tools::tool_is_switchable(&ToolId::Grok));
}

#[test]
fn saving_a_valid_snapshot_resets_known_fields_but_preserves_unknown_fields() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("settings.json");
    std::fs::write(&path, r#"{"providerSyncEnabled":true,"unrelated":{"value":7},"codexAppDreamSkinEnabled":true}"#).unwrap();
    SettingsStore::new(path.clone()).save(&BackendSettings::default()).unwrap();
    let raw: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(raw["providerSyncEnabled"], false);
    assert_eq!(raw["unrelated"], json!({"value":7}));
    assert_eq!(raw["codexAppDreamSkinEnabled"], true);
}

#[test]
fn legacy_theme_backup_notice_never_modifies_host_or_backup() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config.toml");
    let host = b"[desktop]\nappearanceTheme = 'external-edit'\nunrelated = 'keep'\n";
    std::fs::write(&config, host).unwrap();
    let backup = temp.path().join(LEGACY_THEME_BACKUP_FILE);
    assert!(legacy_theme_backup_notice(temp.path()).is_none());
    for bytes in [
        br#"{"schema_version":1,"config_path":"other-existing-config","values":{"appearanceTheme":"old"}}"#.as_slice(),
        br#"{"schema_version":2,"config_path":"missing-config","values":{"appearanceLightChromeTheme":"old"}}"#.as_slice(),
        b"invalid legacy backup".as_slice(),
    ] {
        std::fs::write(&backup, bytes).unwrap();
        for _ in 0..2 {
            assert!(legacy_theme_backup_notice(temp.path()).unwrap().contains("未自动恢复"));
            assert_eq!(std::fs::read(&config).unwrap(), host);
            assert_eq!(std::fs::read(&backup).unwrap(), bytes);
        }
    }
}
