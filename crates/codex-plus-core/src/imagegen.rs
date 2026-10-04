//! Native imagegen-Z8 configuration and model-list helpers.
//!
//! The Manager owns the account-to-skill handoff.  API key material is accepted
//! only by native code; the renderer receives model IDs and redacted state.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use reqwest::redirect::Policy;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::paths::default_imagegen_state_path;
use crate::settings::{atomic_private_write, atomic_write};

pub const DEFAULT_IMAGEGEN_BASE_URL: &str = "https://z8.hk/v1";
pub const NO_IMAGE_MODELS_MESSAGE: &str =
    "当前 API Key 不是生图分组 Key，请前往 Z8 官方后台配置对应的生图 Key 后重试";
/// The relay can add newer image models without requiring a Z8 Codex update.
/// Keep the preferred default in one place and fall back to the first returned
/// image model when a newer/renamed catalog no longer contains these IDs.
pub const DEFAULT_IMAGEGEN_MODEL_PRIORITY: [&str; 3] = [
    "gpt-image-2.5-flare",
    "gpt-image-2.5-sunburst",
    "gpt-image-2",
];
const MAX_RESPONSE_BYTES: usize = 1 << 20;
const MAX_MODEL_ID_BYTES: usize = 256;
const MAX_API_KEY_BYTES: usize = 4096;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImagegenState {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub managed: bool,
    #[serde(default)]
    pub key_id: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub models_observed_at_ms: Option<u64>,
}

impl ImagegenState {
    pub fn fresh() -> Self {
        Self {
            schema_version: 1,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImagegenStatus {
    pub configured: bool,
    pub managed: bool,
    pub key_id: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub models: Vec<String>,
    pub models_observed_at_ms: Option<u64>,
    #[serde(default)]
    pub skill_installed: bool,
    #[serde(default)]
    pub skill_enabled: bool,
    #[serde(default)]
    pub skill_ready: bool,
    #[serde(default)]
    pub skill_managed: bool,
    #[serde(default)]
    pub skill_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImagegenError {
    InvalidInput(&'static str),
    ClientUnavailable,
    Timeout,
    Network,
    Http(u16),
    ResponseTooLarge,
    InvalidResponse,
    Business(String),
}

impl std::fmt::Display for ImagegenError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInput(message) => formatter.write_str(message),
            Self::ClientUnavailable => formatter.write_str("无法创建生图网络客户端"),
            Self::Timeout => formatter.write_str("生图模型列表请求超时，请稍后重试"),
            Self::Network => formatter.write_str("生图模型列表暂时不可用，请稍后重试"),
            Self::Http(status) => write!(formatter, "生图模型列表请求失败（HTTP {status}）"),
            Self::ResponseTooLarge => formatter.write_str("生图模型列表响应过大，暂时无法读取"),
            Self::InvalidResponse => formatter.write_str("生图模型列表返回格式无效"),
            Self::Business(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ImagegenError {}

pub fn default_config_path() -> anyhow::Result<PathBuf> {
    let home = directories::BaseDirs::new()
        .map(|dirs| dirs.home_dir().to_path_buf())
        .ok_or_else(|| anyhow::anyhow!("无法定位用户目录"))?;
    Ok(home.join(".imagegen-Z8").join(".env"))
}

pub fn load_state() -> ImagegenState {
    load_state_from(&default_imagegen_state_path())
}

pub fn load_state_from(path: &Path) -> ImagegenState {
    let Ok(bytes) = fs::read(path) else {
        return ImagegenState::fresh();
    };
    serde_json::from_slice::<ImagegenState>(&bytes).unwrap_or_else(|_| ImagegenState::fresh())
}

pub fn save_state(state: &ImagegenState) -> anyhow::Result<()> {
    save_state_to(&default_imagegen_state_path(), state)
}

pub fn save_state_to(path: &Path, state: &ImagegenState) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec_pretty(state)?;
    atomic_write(path, &bytes)
}

pub fn clear_state() -> anyhow::Result<()> {
    let path = default_imagegen_state_path();
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("清理生图状态失败：{}", path.display())),
    }
}

pub fn status_from_state(state: &ImagegenState) -> ImagegenStatus {
    let config_path = default_config_path().ok();
    ImagegenStatus {
        configured: state.managed
            && config_path.is_some_and(|path| path.is_file())
            && state.key_id.is_some()
            && state.model.is_some(),
        managed: state.managed,
        key_id: state.key_id.clone(),
        base_url: state.base_url.clone(),
        model: state.model.clone(),
        models: state.models.clone(),
        models_observed_at_ms: state.models_observed_at_ms,
        skill_installed: false,
        skill_enabled: false,
        skill_ready: false,
        skill_managed: false,
        skill_version: None,
    }
}

/// Pick the model written to `IMAGEGEN_MODEL` after a catalog refresh.  All
/// returned model IDs remain in `ImagegenState.models`; this only determines
/// the default used by the bundled skill when the user does not pass `--model`.
pub fn choose_default_model(models: &[String]) -> Option<String> {
    for preferred in DEFAULT_IMAGEGEN_MODEL_PRIORITY {
        if let Some(model) = models
            .iter()
            .find(|model| model.eq_ignore_ascii_case(preferred))
        {
            return Some(model.clone());
        }
    }
    models
        .iter()
        .find(|model| is_image_model_id(model))
        .cloned()
}

pub async fn fetch_models(base_url: &str, api_key: &str) -> Result<Vec<String>, ImagegenError> {
    let url = models_url(base_url)?;
    if api_key.is_empty()
        || api_key.len() > MAX_API_KEY_BYTES
        || api_key.chars().any(char::is_control)
    {
        return Err(ImagegenError::InvalidInput("所选生图 API Key 无效"));
    }
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(12))
        .redirect(Policy::none())
        .user_agent("Z8-Codex-Imagegen/1")
        .build()
        .map_err(|_| ImagegenError::ClientUnavailable)?;
    let response = client
        .get(url)
        .bearer_auth(api_key)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                ImagegenError::Timeout
            } else {
                ImagegenError::Network
            }
        })?;
    let status = response.status();
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(ImagegenError::ResponseTooLarge);
    }
    let body = response.bytes().await.map_err(|_| ImagegenError::Network)?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(ImagegenError::ResponseTooLarge);
    }
    let value =
        serde_json::from_slice::<Value>(&body).map_err(|_| ImagegenError::InvalidResponse)?;
    if !status.is_success() {
        return Err(ImagegenError::Http(status.as_u16()));
    }
    if let Some(message) = business_error_message(&value) {
        return Err(ImagegenError::Business(message));
    }
    Ok(parse_model_ids(&value))
}

pub fn models_url(base_url: &str) -> Result<String, ImagegenError> {
    let base_url = base_url.trim().trim_end_matches('/');
    let parsed = reqwest::Url::parse(base_url)
        .map_err(|_| ImagegenError::InvalidInput("生图 Base URL 无效"))?;
    let host = parsed.host_str().unwrap_or_default();
    let is_loopback = host.eq_ignore_ascii_case("localhost")
        || host == "127.0.0.1"
        || host == "[::1]"
        || host == "::1";
    if parsed.username() != ""
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || (parsed.scheme() != "https" && !(parsed.scheme() == "http" && is_loopback))
    {
        return Err(ImagegenError::InvalidInput(
            "生图 Base URL 必须是 HTTPS API 根地址",
        ));
    }
    Ok(format!("{base_url}/models"))
}

pub fn serialize_env(base_url: &str, api_key: &str, model: &str) -> anyhow::Result<Vec<u8>> {
    let base_url = base_url.trim();
    let api_key = api_key.trim();
    let model = model.trim();
    if base_url.is_empty() || api_key.is_empty() || model.is_empty() {
        bail!("生图配置缺少 Base URL、API Key 或模型")
    }
    for (label, value, max) in [
        ("Base URL", base_url, 2048),
        ("API Key", api_key, MAX_API_KEY_BYTES),
        ("模型", model, MAX_MODEL_ID_BYTES),
    ] {
        if value.len() > max || value.chars().any(char::is_control) {
            bail!("生图 {label} 包含无效字符")
        }
    }
    let mut output = String::from("# Managed by Z8 Codex. Do not commit or paste this file.\n");
    output.push_str(&format!("IMAGEGEN_MODE=direct\n"));
    output.push_str(&format!("IMAGEGEN_BASE_URL={}\n", dotenv_value(base_url)?));
    output.push_str(&format!("IMAGEGEN_API_KEY={}\n", dotenv_value(api_key)?));
    output.push_str(&format!("IMAGEGEN_MODEL={}\n", dotenv_value(model)?));
    Ok(output.into_bytes())
}

pub fn write_config(base_url: &str, api_key: &str, model: &str) -> anyhow::Result<PathBuf> {
    let path = default_config_path()?;
    let bytes = serialize_env(base_url, api_key, model)?;
    atomic_private_write(&path, &bytes)?;
    Ok(path)
}

fn dotenv_value(value: &str) -> anyhow::Result<String> {
    if !value
        .chars()
        .any(|character| matches!(character, ' ' | '\t' | '#' | '\'' | '"'))
    {
        return Ok(value.to_string());
    }
    if !value.contains('\'') {
        return Ok(format!("'{value}'"));
    }
    if !value.contains('"') {
        return Ok(format!("\"{value}\""));
    }
    bail!("生图配置值包含不支持的引号")
}

fn parse_model_ids(value: &Value) -> Vec<String> {
    let mut values = Vec::new();
    collect_model_ids(value, &mut values, 0);
    let mut seen = BTreeSet::new();
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_MODEL_ID_BYTES
                && !value.chars().any(char::is_control)
        })
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

fn collect_model_ids(value: &Value, output: &mut Vec<String>, depth: usize) {
    if depth > 4 {
        return;
    }
    match value {
        Value::Array(items) => {
            for item in items {
                collect_model_ids(item, output, depth + 1);
            }
        }
        Value::Object(object) => {
            for key in ["data", "models", "items"] {
                if let Some(nested) = object.get(key) {
                    collect_model_ids(nested, output, depth + 1);
                }
            }
            for key in ["id", "model", "name"] {
                if let Some(value) = object.get(key).and_then(Value::as_str) {
                    let value = value.strip_prefix("models/").unwrap_or(value);
                    output.push(value.to_string());
                    break;
                }
            }
        }
        Value::String(value) => output.push(value.clone()),
        _ => {}
    }
}

fn business_error_message(value: &Value) -> Option<String> {
    let object = value.as_object()?;
    let success = object.get("success").and_then(Value::as_bool);
    let code_is_error = object.get("code").is_some_and(|code| {
        code.as_i64().is_some_and(|code| code >= 400)
            || code
                .as_str()
                .is_some_and(|code| code.parse::<u16>().ok().is_some_and(|code| code >= 400))
    });
    if success == Some(false) || code_is_error {
        return object
            .get("message")
            .or_else(|| object.get("msg"))
            .or_else(|| object.get("error").and_then(|error| error.get("message")))
            .and_then(Value::as_str)
            .map(|message| {
                let message = message.trim();
                if message.len() > 256 || message.chars().any(char::is_control) {
                    "生图模型列表鉴权失败".to_string()
                } else {
                    message.to_string()
                }
            });
    }
    None
}

pub fn current_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// The relay returns a shared model catalog for some accounts.  Keep all
/// returned IDs, but use stable image-model markers to reject a programming /
/// multimodal key before it can be written into the image skill config.
pub fn has_image_models(models: &[String]) -> bool {
    models.iter().any(|model| is_image_model_id(model))
}

fn is_image_model_id(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    [
        "image",
        "dall-e",
        "dalle",
        "flux",
        "imagen",
        "stable-diffusion",
        "sdxl",
        "midjourney",
        "seedream",
        "qwen-image",
        "ideogram",
        "recraft",
        "kling",
    ]
    .iter()
    .any(|marker| model.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_openai_and_wrapped_model_lists_without_duplicates() {
        let models = parse_model_ids(&json!({
            "code": 0,
            "data": [{"id": "image-2"}, {"id": "image-2"}, {"name": "models/image-2.5"}]
        }));
        assert_eq!(models, vec!["image-2", "image-2.5"]);
    }

    #[test]
    fn business_error_is_reported_even_when_http_body_has_data() {
        let error = business_error_message(&json!({
            "success": false,
            "code": 401,
            "msg": "token expired",
            "data": [{"id": "image-2"}]
        }));
        assert_eq!(error.as_deref(), Some("token expired"));
    }

    #[test]
    fn dotenv_serializer_quotes_values_and_never_writes_managed_key_id() {
        let bytes = serialize_env("https://relay.example/v1", "key#with space", "image-2").unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("IMAGEGEN_API_KEY='key#with space'"));
        assert!(!text.contains("IMAGEGEN_KEY_ID"));
    }

    #[test]
    fn rejects_unsafe_base_urls() {
        assert!(models_url("https://relay.example/v1?token=secret").is_err());
        assert!(models_url("http://relay.example/v1").is_err());
        assert!(models_url("http://127.0.0.1:8080/v1").is_ok());
    }

    #[test]
    fn detects_image_models_without_accepting_programming_models() {
        assert!(has_image_models(&[
            "gpt-image-2".to_string(),
            "gpt-image-2.5-flare".to_string(),
        ]));
        assert!(has_image_models(&["gemini-2.5-flash-image".to_string()]));
        assert!(!has_image_models(&[
            "gpt-5.6-sol".to_string(),
            "claude-sonnet".to_string(),
        ]));
    }

    #[test]
    fn chooses_flare_before_other_preferred_models_and_falls_back_to_catalog() {
        let models = vec![
            "gpt-image-2".to_string(),
            "gpt-image-2.5-sunburst".to_string(),
            "gpt-image-2.5-flare".to_string(),
        ];
        assert_eq!(
            choose_default_model(&models).as_deref(),
            Some("gpt-image-2.5-flare")
        );
        assert_eq!(
            choose_default_model(&["image-3".to_string()]).as_deref(),
            Some("image-3")
        );
        assert_eq!(choose_default_model(&["gpt-5.6-sol".to_string()]), None);
    }
}
