use std::io::Write;
use std::process::Command;

use codex_plus_core::assets::{force_chinese_locale_config, injection_script_with_settings};
use codex_plus_core::settings::BackendSettings;

#[test]
fn force_chinese_locale_defaults_to_true() {
    let settings = BackendSettings::default();
    assert!(settings.codex_app_force_chinese_locale);
    assert!(!settings.codex_app_fast_startup);

    let json = serde_json::to_value(&settings).expect("serialize default settings");
    assert_eq!(
        json.get("codexAppForceChineseLocale")
            .and_then(|v| v.as_bool()),
        Some(true),
        "default BackendSettings JSON should include codexAppForceChineseLocale = true"
    );
    assert_eq!(
        json.get("codexAppFastStartup").and_then(|v| v.as_bool()),
        Some(false),
        "default BackendSettings JSON should include codexAppFastStartup = false"
    );
}

#[test]
fn force_chinese_locale_missing_from_old_json_defaults_to_true() {
    let json = serde_json::json!({
        "codexAppPath": "",
        "enhancementsEnabled": true,
    });

    let parsed: BackendSettings = serde_json::from_value(json)
        .expect("old settings JSON without codexAppForceChineseLocale should still load");
    assert!(parsed.codex_app_force_chinese_locale);
    assert!(!parsed.codex_app_fast_startup);
}

#[test]
fn force_chinese_locale_false_round_trips_through_json() {
    let mut settings = BackendSettings::default();
    settings.codex_app_force_chinese_locale = false;

    let json = serde_json::to_value(&settings).expect("serialize");
    assert_eq!(
        json.get("codexAppForceChineseLocale")
            .and_then(|v| v.as_bool()),
        Some(false)
    );

    let parsed: BackendSettings =
        serde_json::from_value(json).expect("deserialize codexAppForceChineseLocale");
    assert!(!parsed.codex_app_force_chinese_locale);
}

#[test]
fn force_chinese_locale_config_reflects_setting() {
    let mut settings = BackendSettings::default();
    assert_eq!(
        force_chinese_locale_config(&settings),
        serde_json::json!({ "enabled": true, "locale": "zh-CN" })
    );

    settings.codex_app_force_chinese_locale = false;
    assert_eq!(
        force_chinese_locale_config(&settings),
        serde_json::json!({ "enabled": false, "locale": "zh-CN" })
    );
}

#[test]
fn injection_script_includes_force_chinese_locale_global_and_patch() {
    let mut settings = BackendSettings::default();
    settings.codex_app_force_chinese_locale = true;
    settings.codex_app_fast_startup = true;
    let script = injection_script_with_settings(0, &settings);
    assert!(script.contains(
        "window.__CODEX_PLUS_FORCE_CHINESE_LOCALE__ = {\"enabled\":true,\"locale\":\"zh-CN\"};"
    ));
    assert!(script.contains(
        "window.__CODEX_PLUS_FAST_STARTUP__ = {\"enabled\":true,\"statsigTimeoutMs\":800};"
    ));
    assert!(script.contains("__codexPlusForceChineseLocaleInstalled"));
    assert!(script.contains("__codexPlusFastStartupInstalled"));
    assert!(script.contains("72216192"));
    assert!(script.contains("enable_i18n"));
    assert!(script.contains("locale_source"));
    assert!(script.contains("vscode://codex/${method}"));
    assert!(script.contains("\"get-setting\""));
    assert!(script.contains("\"set-setting\""));
    assert!(script.contains("{ key: \"localeOverride\", value: locale }"));
    assert!(script.contains("window.location.reload()"));
    assert!(script.contains("if (window.sessionStorage.getItem(localeReloadStorageKey) !== marker) return;"));
    assert!(script.contains("codexPlus.forceChineseLocale.managed.v1"));
    assert!(script.contains("const i18nPatchVersion = \"4\""));
    assert!(script.contains("emitStatsigValuesUpdated"));
    assert!(script.contains("The preload bridge is installed after document-start scripts."));
    assert!(!script.contains(
        "window.self !== window || !window.electronBridge || !/^app:\\/\\/\\-\\//i.test"
    ));
    assert!(!script.contains("setItem(\"localeOverride\""));

    settings.codex_app_force_chinese_locale = false;
    let script = injection_script_with_settings(0, &settings);
    assert!(script.contains(
        "window.__CODEX_PLUS_FORCE_CHINESE_LOCALE__ = {\"enabled\":false,\"locale\":\"zh-CN\"};"
    ));
}

#[test]
fn force_chinese_locale_patches_late_statsig_instances_and_i18n_config() {
    let temp = tempfile::tempdir().expect("temp dir should be created");
    let script_path = temp.path().join("renderer-inject.js");
    let harness_path = temp.path().join("force-chinese-locale-harness.cjs");
    let script = injection_script_with_settings(0, &BackendSettings::default());
    std::fs::write(&script_path, script).expect("injection script should be written");

    let script_path = serde_json::to_string(&script_path.to_string_lossy().to_string())
        .expect("script path should serialize");
    let harness = r#"
const scriptPath = __SCRIPT_PATH__;
const storage = new Map();
const timers = [];
function node() {
  return {
    appendChild() {}, prepend() {}, remove() {}, setAttribute() {}, removeAttribute() {},
    addEventListener() {}, querySelector() { return null; }, querySelectorAll() { return []; },
    closest() { return null; }, getAttribute() { return null; },
    classList: { add() {}, remove() {}, toggle() {}, contains() { return false; } },
    dataset: {}, style: {}, children: [], isConnected: true, textContent: "", innerHTML: "",
  };
}
globalThis.window = globalThis;
window.__CODEX_PLUS_TEST_FORCE_CHINESE_LOCALE__ = true;
window.__CODEX_PLUS_FORCE_CHINESE_LOCALE__ = { enabled: true, locale: "zh-CN" };
window.__STATSIG__ = undefined;
window.addEventListener = () => {};
window.removeEventListener = () => {};
window.dispatchEvent = () => true;
globalThis.MutationObserver = class { observe() {} disconnect() {} };
globalThis.ResizeObserver = class { observe() {} disconnect() {} };
globalThis.IntersectionObserver = class { observe() {} disconnect() {} };
globalThis.requestAnimationFrame = (callback) => setTimeout(callback, 0);
globalThis.cancelAnimationFrame = (id) => clearTimeout(id);
globalThis.setTimeout = (callback) => { timers.push(callback); return timers.length; };
globalThis.clearTimeout = () => {};
globalThis.setInterval = () => 0;
globalThis.clearInterval = () => {};
globalThis.document = {
  scripts: [], documentElement: node(), body: node(), createElement: () => node(),
  getElementById: () => null, querySelector: () => null, querySelectorAll: () => [],
  addEventListener() {}, removeEventListener() {},
};
globalThis.localStorage = {
  getItem: (key) => storage.has(key) ? storage.get(key) : null,
  setItem: (key, value) => storage.set(key, String(value)),
  removeItem: (key) => storage.delete(key),
};
globalThis.sessionStorage = globalThis.localStorage;
globalThis.location = { href: "app://-/index.html", pathname: "/index.html", search: "", hash: "", reload() {} };
window.location = globalThis.location;
globalThis.navigator = { userAgent: "node-test", sendBeacon: () => false };
globalThis.performance = { getEntriesByType: () => [] };
globalThis.fetch = async () => ({ ok: true, json: async () => ({}) });
require(scriptPath);

const api = window.__codexPlusForceChineseLocaleTest;
if (!api) process.exit(2);
const makeClient = () => ({
  events: 0,
  $emt() { this.events += 1; },
  getDynamicConfig() {
    const config = { value: { enable_i18n: false, locale_source: "IDE" } };
    config.get = (key, fallback) => Object.prototype.hasOwnProperty.call(config.value, key)
      ? config.value[key]
      : fallback;
    return config;
  },
});
const firstClient = makeClient();
const root = {
  firstInstance: undefined,
  instance() { return firstClient; },
  instances: {},
};
window.__STATSIG__ = root;
api.patchStatsigRoot(root);
root.firstInstance = firstClient;
const firstConfig = root.instance().getDynamicConfig("72216192");

const lateClient = makeClient();
root.instance = () => lateClient;
const lateConfig = root.instance().getDynamicConfig("72216192");
const cases = {
  patchedFlag: window.__codexPlusForceChineseLocaleI18nPatched === true,
  firstEnableI18n: firstConfig.get("enable_i18n", false),
  firstLocaleSource: firstConfig.get("locale_source", "IDE"),
  lateEnableI18n: lateConfig.get("enable_i18n", false),
  lateLocaleSource: lateConfig.get("locale_source", "IDE"),
  lateClientPatched: lateClient.__codexPlusForceChineseLocalePatched === true,
  firstClientReceivedValuesUpdated: firstClient.events > 0,
  lateClientReceivedValuesUpdated: lateClient.events > 0,
};
process.stdout.write(JSON.stringify(cases));
"#.replace("__SCRIPT_PATH__", &script_path);
    let mut file = std::fs::File::create(&harness_path).expect("harness should be created");
    file.write_all(harness.as_bytes())
        .expect("harness should be written");

    let output = Command::new("node")
        .arg(&harness_path)
        .output()
        .expect("node should execute force Chinese locale harness");
    assert!(
        output.status.success(),
        "force Chinese locale harness failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cases: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("force Chinese locale harness should emit JSON");
    assert_eq!(cases["patchedFlag"], true);
    assert_eq!(cases["firstEnableI18n"], true);
    assert_eq!(cases["firstLocaleSource"], "SYSTEM");
    assert_eq!(cases["lateEnableI18n"], true);
    assert_eq!(cases["lateLocaleSource"], "SYSTEM");
    assert_eq!(cases["lateClientPatched"], true);
    assert_eq!(cases["firstClientReceivedValuesUpdated"], true);
    assert_eq!(cases["lateClientReceivedValuesUpdated"], true);
}
