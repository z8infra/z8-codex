//! Pure data and transformation model for the Z8 Provider.
//!
//! This module intentionally does not know about Z8 Launch processes,
//! managed `CODEX_HOME`, Tauri commands, or network clients.  It owns the
//! boundary between an authenticated Z8 account and Codex++ configuration:
//! key summaries and selection, safe provider defaults, health-check result
//! mapping, and an all-or-nothing configuration update plan.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::fmt;
use toml_edit::{DocumentMut, Item};

pub const Z8_PROVIDER_ID: &str = "z8";
pub const Z8_PROVIDER_NAME: &str = "Z8";
pub const Z8_BASE_URL: &str = "https://z8.hk/v1";
pub const Z8_WIRE_API: &str = "responses";
pub const Z8_DEFAULT_MODEL: &str = "gpt-6-astra";
/// Models shown in a fresh Z8 profile when the upstream model endpoint is unavailable.
///
/// Keep this list in provider-profile storage (rather than the TOML template) so the
/// manager can render and later replace it without overwriting the user's other
/// provider settings.
pub const Z8_DEFAULT_MODEL_LIST: &str =
    "gpt-6.1-sol\ngpt-6-astra\ngpt-6-sol\ngpt-6-luna\ngpt-5.6-sol\ngpt-5.6-terra\ngpt-5.6-luna\ngpt-5.5";
pub const Z8_API_KEY_ENV: &str = "OPENAI_API_KEY";
pub const Z8_DEFAULT_PROFILE_CONFIG: &str = "model = \"gpt-6-astra\"\nmodel_provider = \"custom\"\n\n[features]\ngoals = true\n\n[model_providers.custom]\nname = \"custom\"\nwire_api = \"responses\"\nrequires_openai_auth = true\nbase_url = \"https://z8.hk/v1\"\n";

const MAX_API_KEY_BYTES: usize = 4096;
const MAX_PROVIDER_ID_BYTES: usize = 64;
const MAX_MODEL_BYTES: usize = 256;

/// A secret API key that is deliberately not serializable or displayable.
///
/// Callers should keep this value in native code and only borrow it while
/// constructing an authenticated request or a configuration write plan.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretApiKey(String);

impl SecretApiKey {
    pub fn new(value: impl Into<String>) -> Result<Self, ProvisioningError> {
        let value = value.into();
        validate_api_key(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn summary(&self) -> ApiKeySecretSummary {
        ApiKeySecretSummary {
            masked: mask_api_key(&self.0),
            fingerprint: fingerprint(&self.0),
        }
    }
}

impl fmt::Debug for SecretApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretApiKey(REDACTED)")
    }
}

/// Non-sensitive information safe for Manager UI and diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeySecretSummary {
    pub masked: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ApiKeyStatus {
    #[default]
    Active,
    Disabled,
    Revoked,
    Expired,
    Unknown,
}

impl ApiKeyStatus {
    pub fn is_usable(self) -> bool {
        matches!(self, Self::Active)
    }
}

impl From<&str> for ApiKeyStatus {
    fn from(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "active" | "enabled" | "available" => Self::Active,
            "disabled" | "inactive" => Self::Disabled,
            "revoked" | "revoke" => Self::Revoked,
            "expired" | "expire" => Self::Expired,
            _ => Self::Unknown,
        }
    }
}

/// A Key record returned by the account service.  It must never contain the
/// original secret; `masked` and `fingerprint` are the only UI-safe hints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeySummary {
    pub id: String,
    pub name: String,
    pub status: ApiKeyStatus,
    pub secret: ApiKeySecretSummary,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub selected: bool,
}

impl ApiKeySummary {
    pub fn is_usable(&self) -> bool {
        self.status.is_usable()
    }
}

pub fn summarize_api_key(
    id: impl Into<String>,
    name: impl Into<String>,
    status: ApiKeyStatus,
    secret: &SecretApiKey,
) -> ApiKeySummary {
    ApiKeySummary {
        id: id.into(),
        name: name.into(),
        status,
        secret: secret.summary(),
        created_at: None,
        expires_at: None,
        selected: false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "value")]
pub enum ApiKeySelection {
    Id(String),
    FirstUsable,
}

pub fn select_api_key<'a>(
    keys: &'a [ApiKeySummary],
    selection: &ApiKeySelection,
) -> Result<&'a ApiKeySummary, ProvisioningError> {
    let selected = match selection {
        ApiKeySelection::Id(id) => keys.iter().find(|key| key.id == *id),
        ApiKeySelection::FirstUsable => keys.iter().find(|key| key.is_usable()),
    };
    let key = selected.ok_or_else(|| match selection {
        ApiKeySelection::Id(id) => ProvisioningError::KeyNotFound { id: id.clone() },
        ApiKeySelection::FirstUsable => ProvisioningError::NoUsableKey,
    })?;
    if !key.is_usable() {
        return Err(ProvisioningError::KeyNotUsable {
            id: key.id.clone(),
            status: key.status,
        });
    }
    Ok(key)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Z8ProviderConfig {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub wire_api: String,
    pub api_key_env: String,
    pub models: Vec<String>,
    pub default_model: String,
}

impl Default for Z8ProviderConfig {
    fn default() -> Self {
        Self {
            id: Z8_PROVIDER_ID.to_string(),
            name: Z8_PROVIDER_NAME.to_string(),
            base_url: Z8_BASE_URL.to_string(),
            wire_api: Z8_WIRE_API.to_string(),
            api_key_env: Z8_API_KEY_ENV.to_string(),
            models: vec![Z8_DEFAULT_MODEL.to_string()],
            default_model: Z8_DEFAULT_MODEL.to_string(),
        }
    }
}

impl Z8ProviderConfig {
    pub fn health_endpoint(&self) -> String {
        format!("{}/models", self.base_url.trim_end_matches('/'))
    }

    fn validate(&self) -> Result<(), ProvisioningError> {
        validate_non_empty_bounded(&self.id, MAX_PROVIDER_ID_BYTES, "provider id")?;
        validate_non_empty_bounded(&self.default_model, MAX_MODEL_BYTES, "default model")?;
        if self.base_url.trim().is_empty() || !self.base_url.starts_with("https://") {
            return Err(ProvisioningError::InvalidProvider {
                reason: "provider base URL must use HTTPS".to_string(),
            });
        }
        if self.models.is_empty() || !self.models.iter().any(|model| model == &self.default_model) {
            return Err(ProvisioningError::InvalidProvider {
                reason: "default model must be present in the model list".to_string(),
            });
        }
        Ok(())
    }
}

/// Keep profile-local Codex settings while replacing Z8-owned routing fields.
pub fn z8_profile_config_contents(existing: &str) -> String {
    if existing.trim().is_empty() {
        return Z8_DEFAULT_PROFILE_CONFIG.to_string();
    }
    let Ok(mut document) = existing.parse::<DocumentMut>() else {
        return Z8_DEFAULT_PROFILE_CONFIG.to_string();
    };
    let root = document.as_table_mut();
    for key in [
        "model_providers",
        "profile",
        "profiles",
        "base_url",
        "wire_api",
        "env_key",
        "api_key",
        "experimental_bearer_token",
        "openai_base_url",
        "chatgpt_base_url",
        "codex_plus_chat_base_url",
        "model_catalog_json",
    ] {
        root.remove(key);
    }
    document["model_provider"] = toml_edit::value("custom");
    if document
        .get("features")
        .and_then(Item::as_table_like)
        .is_none()
    {
        document["features"] = toml_edit::table();
    }
    if document["features"]
        .get("goals")
        .and_then(Item::as_bool)
        .is_none()
    {
        document["features"]["goals"] = toml_edit::value(true);
    }
    let mut contents = document.to_string();
    if !contents.ends_with('\n') {
        contents.push('\n');
    }
    contents
}

/// Replace only the API key in a Z8 auth document while preserving any other
/// metadata the user or Codex has stored alongside it.
pub fn set_z8_api_key_in_auth_contents(existing: &str, api_key: &str) -> anyhow::Result<String> {
    let mut value = if existing.trim().is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::from_str::<Value>(existing)?
    };
    let object = value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("auth.json 必须是 JSON 对象"))?;
    object.insert(
        "OPENAI_API_KEY".to_string(),
        Value::String(api_key.to_string()),
    );
    object
        .entry("auth_mode".to_string())
        .or_insert_with(|| Value::String("apikey".to_string()));
    Ok(format!("{}\n", serde_json::to_string_pretty(&value)?))
}

/// Return whether Codex++ has the Z8 Provider documents that the Manager
/// writes after a key is applied.
///
/// This is deliberately a pure check over document contents.  The launcher
/// can use it after reading the configured home, while tests can exercise the
/// entry gate with in-memory fixtures and never touch a user's `auth.json`.
/// A key is considered applied only when the active provider is the Z8
/// provider, its provider table points at the expected Z8 transport, and the
/// auth document contains the expected API key.  The optional expected key is
/// compared exactly so a key from an older account cannot authorize a direct
/// launch after the user switches accounts.
pub fn z8_provider_documents_are_applied(
    config_toml: &str,
    auth_json: &str,
    expected_api_key: Option<&str>,
) -> bool {
    let Ok(document) = config_toml.parse::<DocumentMut>() else {
        return false;
    };
    let root_provider = document
        .get("model_provider")
        .and_then(Item::as_value)
        .and_then(toml_edit::Value::as_str)
        .map(str::trim);
    let profile_provider = document
        .get("profile")
        .and_then(Item::as_value)
        .and_then(toml_edit::Value::as_str)
        .map(str::trim)
        .and_then(|profile_name| {
            document
                .get("profiles")
                .and_then(Item::as_table_like)
                .and_then(|profiles| profiles.get(profile_name))
                .and_then(Item::as_table_like)
                .and_then(|profile| profile.get("model_provider"))
                .and_then(Item::as_value)
                .and_then(toml_edit::Value::as_str)
                .map(str::trim)
        });
    let active_provider = profile_provider.or(root_provider);
    if active_provider != Some(Z8_PROVIDER_ID) {
        return false;
    }

    let Some(provider) = document
        .get("model_providers")
        .and_then(Item::as_table_like)
        .and_then(|providers| providers.get(Z8_PROVIDER_ID))
        .and_then(Item::as_table_like)
    else {
        return false;
    };
    let provider_value = |key: &str| {
        provider
            .get(key)
            .and_then(Item::as_value)
            .and_then(toml_edit::Value::as_str)
            .map(str::trim)
    };
    if provider_value("wire_api") != Some(Z8_WIRE_API)
        || provider_value("env_key") != Some(Z8_API_KEY_ENV)
        || provider_value("base_url")
            .map(|value| value.trim_end_matches('/'))
            != Some(Z8_BASE_URL.trim_end_matches('/'))
    {
        return false;
    }

    let Ok(auth) = serde_json::from_str::<Value>(auth_json) else {
        return false;
    };
    let Some(auth) = auth.as_object() else {
        return false;
    };
    if auth
        .get("auth_mode")
        .and_then(Value::as_str)
        .map(str::trim)
        != Some("apikey")
    {
        return false;
    }
    let Some(api_key) = auth
        .get(Z8_API_KEY_ENV)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return false;
    };
    expected_api_key.is_none_or(|expected| !expected.is_empty() && expected == api_key)
}

#[derive(Clone, PartialEq, Eq)]
pub struct ProviderConfigUpdate {
    pub provider: Z8ProviderConfig,
    pub selected_key_id: String,
    pub secret: SecretApiKey,
    pub expected_revision: Option<String>,
}

impl ProviderConfigUpdate {
    pub fn new(
        provider: Z8ProviderConfig,
        selected_key_id: impl Into<String>,
        secret: SecretApiKey,
    ) -> Result<Self, ProvisioningError> {
        provider.validate()?;
        let selected_key_id = selected_key_id.into();
        validate_non_empty_bounded(&selected_key_id, MAX_PROVIDER_ID_BYTES, "key id")?;
        Ok(Self {
            provider,
            selected_key_id,
            secret,
            expected_revision: None,
        })
    }
}

impl fmt::Debug for ProviderConfigUpdate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderConfigUpdate")
            .field("provider", &self.provider)
            .field("selected_key_id", &self.selected_key_id)
            .field("secret", &"REDACTED")
            .field("expected_revision", &self.expected_revision)
            .finish()
    }
}

/// The two documents that Codex++ needs to update.  This is intentionally a
/// value object so callers can compare the revision before committing either
/// document and can use the existing native atomic writer for the actual I/O.
#[derive(Clone, PartialEq, Eq)]
pub struct ProviderConfigSnapshot {
    pub config_toml: String,
    pub auth_json: String,
    pub revision: String,
}

impl ProviderConfigSnapshot {
    pub fn new(config_toml: impl Into<String>, auth_json: impl Into<String>) -> Self {
        let config_toml = config_toml.into();
        let auth_json = auth_json.into();
        let revision = revision_for_documents(&config_toml, &auth_json);
        Self {
            config_toml,
            auth_json,
            revision,
        }
    }
}

impl fmt::Debug for ProviderConfigSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderConfigSnapshot")
            .field("config_toml_bytes", &self.config_toml.len())
            .field("auth_json", &"REDACTED")
            .field("revision", &self.revision)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ProviderConfigUpdatePlan {
    pub previous_revision: String,
    pub next_revision: String,
    pub config_toml: String,
    pub auth_json: String,
    pub provider_id: String,
    pub selected_key_id: String,
}

impl fmt::Debug for ProviderConfigUpdatePlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderConfigUpdatePlan")
            .field("previous_revision", &self.previous_revision)
            .field("next_revision", &self.next_revision)
            .field("config_toml_bytes", &self.config_toml.len())
            .field("auth_json", &"REDACTED")
            .field("provider_id", &self.provider_id)
            .field("selected_key_id", &self.selected_key_id)
            .finish()
    }
}

impl ProviderConfigUpdatePlan {
    pub fn verify_against(
        &self,
        current: &ProviderConfigSnapshot,
    ) -> Result<(), ProvisioningError> {
        if current.revision != self.previous_revision {
            return Err(ProvisioningError::RevisionMismatch {
                expected: self.previous_revision.clone(),
                actual: current.revision.clone(),
            });
        }
        Ok(())
    }

    pub fn snapshot(&self) -> ProviderConfigSnapshot {
        ProviderConfigSnapshot::new(self.config_toml.clone(), self.auth_json.clone())
    }
}

/// A compare-and-prepare transaction for the two Codex++ config documents.
///
/// The transaction deliberately stops before filesystem I/O.  The caller
/// must verify the live snapshot and then write `commit()`'s returned pair
/// with Codex++'s existing atomic writer.  This keeps the provisioning model
/// independent of Z8 Launch and prevents a stale UI request from overwriting
/// a newer provider selection.
#[derive(Clone, PartialEq, Eq)]
pub struct ProviderConfigTransaction {
    plan: ProviderConfigUpdatePlan,
}

impl fmt::Debug for ProviderConfigTransaction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderConfigTransaction")
            .field("plan", &self.plan)
            .finish()
    }
}

impl ProviderConfigTransaction {
    pub fn begin(
        current: &ProviderConfigSnapshot,
        update: &ProviderConfigUpdate,
    ) -> Result<Self, ProvisioningError> {
        Ok(Self {
            plan: prepare_provider_config_update(current, update)?,
        })
    }

    pub fn plan(&self) -> &ProviderConfigUpdatePlan {
        &self.plan
    }

    pub fn commit(
        self,
        live: &ProviderConfigSnapshot,
    ) -> Result<ProviderConfigSnapshot, ProvisioningError> {
        self.plan.verify_against(live)?;
        Ok(self.plan.snapshot())
    }
}

pub fn prepare_provider_config_update(
    current: &ProviderConfigSnapshot,
    update: &ProviderConfigUpdate,
) -> Result<ProviderConfigUpdatePlan, ProvisioningError> {
    update.provider.validate()?;
    if let Some(expected) = update.expected_revision.as_deref() {
        if expected != current.revision {
            return Err(ProvisioningError::RevisionMismatch {
                expected: expected.to_string(),
                actual: current.revision.clone(),
            });
        }
    }

    let mut document = current.config_toml.parse::<DocumentMut>().map_err(|_| {
        ProvisioningError::InvalidConfig {
            document: "config.toml".to_string(),
        }
    })?;
    document["model_provider"] = toml_edit::value(update.provider.id.clone());
    // Codex applies an active profile after the root table. Keep that override
    // aligned with the root provider so a stale profile cannot route the next
    // launch back to an older upstream.
    let active_profile = document
        .get("profile")
        .and_then(Item::as_value)
        .and_then(toml_edit::Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToString::to_string);
    if let Some(profile_name) = active_profile {
        if let Some(profiles) = document
            .get_mut("profiles")
            .and_then(Item::as_table_mut)
        {
            if let Some(profile) = profiles
                .get_mut(&profile_name)
                .and_then(Item::as_table_mut)
            {
                profile["model_provider"] = toml_edit::value(update.provider.id.clone());
            }
        }
    }
    // Selecting a different API key must not change the model the user is
    // currently working with.  A fresh or incomplete config still receives
    // the provider's default model so the first launch remains usable.
    let has_model = document
        .get("model")
        .and_then(Item::as_value)
        .and_then(toml_edit::Value::as_str)
        .is_some_and(|model| !model.trim().is_empty());
    if !has_model {
        document["model"] = toml_edit::value(update.provider.default_model.clone());
    }
    document["preferred_auth_method"] = toml_edit::value("apikey");
    document["forced_login_method"] = toml_edit::value("api");
    document["cli_auth_credentials_store"] = toml_edit::value("file");
    let providers = ensure_table(&mut document, "model_providers")?;
    let provider = ensure_nested_table(providers, &update.provider.id)?;
    provider["name"] = toml_edit::value(update.provider.name.clone());
    provider["wire_api"] = toml_edit::value(update.provider.wire_api.clone());
    provider["base_url"] = toml_edit::value(update.provider.base_url.clone());
    provider["env_key"] = toml_edit::value(update.provider.api_key_env.clone());
    let config_toml = ensure_trailing_newline(document.to_string());

    let mut auth = parse_auth_document(&current.auth_json)?;
    auth.insert("auth_mode".to_string(), Value::String("apikey".to_string()));
    auth.insert(
        update.provider.api_key_env.clone(),
        Value::String(update.secret.as_str().to_string()),
    );
    let auth_json = serde_json::to_string_pretty(&Value::Object(auth)).map_err(|_| {
        ProvisioningError::InvalidConfig {
            document: "auth.json".to_string(),
        }
    })? + "\n";

    Ok(ProviderConfigUpdatePlan {
        previous_revision: current.revision.clone(),
        next_revision: revision_for_documents(&config_toml, &auth_json),
        config_toml,
        auth_json,
        provider_id: update.provider.id.clone(),
        selected_key_id: update.selected_key_id.clone(),
    })
}

fn ensure_table<'a>(
    document: &'a mut DocumentMut,
    key: &str,
) -> Result<&'a mut toml_edit::Table, ProvisioningError> {
    if document.get(key).is_none() {
        document[key] = toml_edit::table();
    }
    document
        .get_mut(key)
        .and_then(Item::as_table_mut)
        .ok_or_else(|| ProvisioningError::InvalidConfig {
            document: format!("config.toml [{key}]"),
        })
}

fn ensure_nested_table<'a>(
    table: &'a mut toml_edit::Table,
    key: &str,
) -> Result<&'a mut toml_edit::Table, ProvisioningError> {
    if table.get(key).is_none() {
        table[key] = toml_edit::table();
    }
    table
        .get_mut(key)
        .and_then(Item::as_table_mut)
        .ok_or_else(|| ProvisioningError::InvalidConfig {
            document: format!("config.toml [model_providers.{key}]"),
        })
}

fn parse_auth_document(contents: &str) -> Result<Map<String, Value>, ProvisioningError> {
    if contents.trim().is_empty() {
        return Ok(Map::new());
    }
    let value =
        serde_json::from_str::<Value>(contents).map_err(|_| ProvisioningError::InvalidConfig {
            document: "auth.json".to_string(),
        })?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| ProvisioningError::InvalidConfig {
            document: "auth.json".to_string(),
        })
}

fn ensure_trailing_newline(mut value: String) -> String {
    if !value.ends_with('\n') {
        value.push('\n');
    }
    value
}

pub fn revision_for_documents(config_toml: &str, auth_json: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update((config_toml.len() as u64).to_le_bytes());
    hasher.update(config_toml.as_bytes());
    hasher.update((auth_json.len() as u64).to_le_bytes());
    hasher.update(auth_json.as_bytes());
    hex_digest(&hasher.finalize())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthCheckStatus {
    Healthy,
    AuthenticationFailed,
    KeyInvalid,
    ModelUnavailable,
    NetworkFailed,
    ServerError,
    NotConfigured,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthCheckError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub http_status: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthCheckResult {
    pub provider_id: String,
    pub endpoint: String,
    pub model: String,
    pub status: HealthCheckStatus,
    pub latency_ms: Option<u64>,
    #[serde(default)]
    pub error: Option<HealthCheckError>,
}

pub fn health_check_from_http(
    provider: &Z8ProviderConfig,
    model: impl Into<String>,
    http_status: u16,
    latency_ms: Option<u64>,
) -> HealthCheckResult {
    let model = model.into();
    let (status, error) = match http_status {
        200..=299 => (HealthCheckStatus::Healthy, None),
        401 => (
            HealthCheckStatus::KeyInvalid,
            Some(safe_health_error(
                "provider_key_invalid",
                "Z8 API Key 无效。",
                http_status,
            )),
        ),
        403 => (
            HealthCheckStatus::AuthenticationFailed,
            Some(safe_health_error(
                "provider_forbidden",
                "账户无权访问 Z8 Provider。",
                http_status,
            )),
        ),
        404 => (
            HealthCheckStatus::ModelUnavailable,
            Some(safe_health_error(
                "provider_model_unavailable",
                "请求的模型或健康检查接口不可用。",
                http_status,
            )),
        ),
        408 | 429 => (
            HealthCheckStatus::NetworkFailed,
            Some(safe_health_error(
                "provider_retryable",
                "Z8 Provider 暂时无法完成检查，请稍后重试。",
                http_status,
            )),
        ),
        500..=599 => (
            HealthCheckStatus::ServerError,
            Some(safe_health_error(
                "provider_server_error",
                "Z8 Provider 服务端返回错误。",
                http_status,
            )),
        ),
        _ => (
            HealthCheckStatus::ServerError,
            Some(safe_health_error(
                "provider_request_failed",
                "Z8 Provider 检查失败。",
                http_status,
            )),
        ),
    };
    HealthCheckResult {
        provider_id: provider.id.clone(),
        endpoint: provider.health_endpoint(),
        model,
        status,
        latency_ms,
        error,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthTransportError {
    InvalidUrl,
    Timeout,
    Network,
    InvalidResponse,
}

pub fn health_check_from_transport_error(
    provider: &Z8ProviderConfig,
    model: impl Into<String>,
    error: HealthTransportError,
) -> HealthCheckResult {
    let (code, message) = match error {
        HealthTransportError::InvalidUrl => ("provider_base_url_invalid", "Z8 Provider 地址无效。"),
        HealthTransportError::Timeout => ("provider_timeout", "Z8 Provider 检查超时。"),
        HealthTransportError::Network => ("provider_unavailable", "无法连接 Z8 Provider。"),
        HealthTransportError::InvalidResponse => {
            ("provider_invalid_response", "Z8 Provider 返回了无效响应。")
        }
    };
    HealthCheckResult {
        provider_id: provider.id.clone(),
        endpoint: provider.health_endpoint(),
        model: model.into(),
        status: HealthCheckStatus::NetworkFailed,
        latency_ms: None,
        error: Some(HealthCheckError {
            code: code.to_string(),
            message: message.to_string(),
            http_status: None,
        }),
    }
}

fn safe_health_error(code: &str, message: &str, http_status: u16) -> HealthCheckError {
    HealthCheckError {
        code: code.to_string(),
        message: message.to_string(),
        http_status: Some(http_status),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProvisioningError {
    #[error("API Key 为空或格式无效")]
    InvalidApiKey,
    #[error("没有可用的 API Key")]
    NoUsableKey,
    #[error("API Key 不存在: {id}")]
    KeyNotFound { id: String },
    #[error("API Key 不可用: {id} ({status:?})")]
    KeyNotUsable { id: String, status: ApiKeyStatus },
    #[error("Provider 配置无效: {reason}")]
    InvalidProvider { reason: String },
    #[error("{document} 无效")]
    InvalidConfig { document: String },
    #[error("Provider 配置已被修改，请刷新后重试")]
    RevisionMismatch { expected: String, actual: String },
}

fn validate_api_key(value: &str) -> Result<(), ProvisioningError> {
    if value.is_empty()
        || value.len() > MAX_API_KEY_BYTES
        || value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(ProvisioningError::InvalidApiKey);
    }
    Ok(())
}

fn validate_non_empty_bounded(
    value: &str,
    max_bytes: usize,
    label: &str,
) -> Result<(), ProvisioningError> {
    if value.trim().is_empty()
        || value.len() > max_bytes
        || value.chars().any(|character| character.is_control())
    {
        return Err(ProvisioningError::InvalidProvider {
            reason: format!("{label} is empty or too long"),
        });
    }
    Ok(())
}

fn mask_api_key(value: &str) -> String {
    let tail = value
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("••••{tail}")
}

fn fingerprint(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    hex_digest(&digest[..6])
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret() -> SecretApiKey {
        SecretApiKey::new("z8_test_key_1234").unwrap()
    }

    #[test]
    fn secret_debug_and_summary_never_expose_full_key() {
        let key = secret();
        let debug = format!("{key:?}");
        let summary = serde_json::to_string(&key.summary()).unwrap();
        assert!(!debug.contains(key.as_str()));
        assert!(!summary.contains(key.as_str()));
        assert_eq!(key.summary().masked, "••••1234");
        assert_eq!(key.summary().fingerprint.len(), 12);
    }

    #[test]
    fn selection_rejects_non_usable_key() {
        let mut revoked = summarize_api_key("revoked", "Old", ApiKeyStatus::Revoked, &secret());
        revoked.selected = true;
        assert_eq!(
            select_api_key(&[revoked], &ApiKeySelection::FirstUsable),
            Err(ProvisioningError::NoUsableKey)
        );
    }

    #[test]
    fn default_provider_is_z8_and_has_a_default_model() {
        let provider = Z8ProviderConfig::default();
        assert_eq!(provider.id, Z8_PROVIDER_ID);
        assert_eq!(provider.base_url, Z8_BASE_URL);
        assert_eq!(provider.default_model, "gpt-6-astra");
        assert_eq!(provider.health_endpoint(), "https://z8.hk/v1/models");
        assert!(provider.models.contains(&provider.default_model));
    }

    #[test]
    fn default_model_list_contains_the_common_codex_models() {
        assert_eq!(
            Z8_DEFAULT_MODEL_LIST.lines().collect::<Vec<_>>(),
            vec![
                "gpt-6.1-sol",
                "gpt-6-astra",
                "gpt-6-sol",
                "gpt-6-luna",
                "gpt-5.6-sol",
                "gpt-5.6-terra",
                "gpt-5.6-luna",
                "gpt-5.5",
            ]
        );
        assert!(Z8_DEFAULT_MODEL_LIST.lines().any(|model| model == Z8_DEFAULT_MODEL));
    }

    #[test]
    fn z8_profile_defaults_enable_goals_and_keep_local_codex_settings() {
        let fresh = z8_profile_config_contents("");
        assert_eq!(fresh, Z8_DEFAULT_PROFILE_CONFIG);

        let disabled = z8_profile_config_contents(
            "model_provider = \"codex_local_access\"\nmodel = \"gpt-6-astra\"\nsandbox_mode = \"workspace-write\"\nprofile = \"legacy\"\n\n[features]\ngoals = false\nfast_mode = true\n\n[model_providers.codex_local_access]\nbase_url = \"https://old.example/v1\"\nexperimental_bearer_token = \"old-secret\"\n\n[mcp_servers.example]\ncommand = \"example\"\n",
        );
        let document = disabled.parse::<DocumentMut>().unwrap();
        assert_eq!(document["model_provider"].as_str(), Some("custom"));
        assert_eq!(document["model"].as_str(), Some("gpt-6-astra"));
        assert_eq!(document["sandbox_mode"].as_str(), Some("workspace-write"));
        assert_eq!(document["features"]["goals"].as_bool(), Some(false));
        assert_eq!(document["features"]["fast_mode"].as_bool(), Some(true));
        assert_eq!(document["mcp_servers"]["example"]["command"].as_str(), Some("example"));
        assert!(document.get("model_providers").is_none());
        assert!(document.get("profile").is_none());
        assert!(!disabled.contains("codex_local_access"));
        assert!(!disabled.contains("old-secret"));
    }

    #[test]
    fn switching_z8_keys_preserves_auth_metadata() {
        let updated = set_z8_api_key_in_auth_contents(
            r#"{"auth_mode":"apikey","last_refresh":"keep-me","OPENAI_API_KEY":"old"}"#,
            "new-key",
        )
        .unwrap();
        let document: Value = serde_json::from_str(&updated).unwrap();
        assert_eq!(document["OPENAI_API_KEY"], "new-key");
        assert_eq!(document["auth_mode"], "apikey");
        assert_eq!(document["last_refresh"], "keep-me");
    }

    #[test]
    fn z8_provider_documents_require_matching_applied_key() {
        let config = r#"
model_provider = "z8"
model = "gpt-5.6-sol"
preferred_auth_method = "apikey"
forced_login_method = "api"
cli_auth_credentials_store = "file"

[model_providers.z8]
name = "Z8"
wire_api = "responses"
base_url = "https://z8.hk/v1"
env_key = "OPENAI_API_KEY"
"#;
        let auth = r#"{"auth_mode":"apikey","OPENAI_API_KEY":"z8_test_key_1234"}"#;

        assert!(z8_provider_documents_are_applied(
            config,
            auth,
            Some("z8_test_key_1234")
        ));
        assert!(z8_provider_documents_are_applied(config, auth, None));
        assert!(!z8_provider_documents_are_applied(
            config,
            auth,
            Some("z8_other_key_5678")
        ));
        assert!(!z8_provider_documents_are_applied(
            config,
            r#"{"auth_mode":"chatgpt","OPENAI_API_KEY":"z8_test_key_1234"}"#,
            Some("z8_test_key_1234")
        ));
        assert!(!z8_provider_documents_are_applied(
            &config.replace("model_provider = \"z8\"", "model_provider = \"openai\""),
            auth,
            Some("z8_test_key_1234")
        ));
    }

    #[test]
    fn config_update_is_secret_free_in_toml_and_revision_guarded() {
        let current = ProviderConfigSnapshot::new(
            "model_provider = \"openai\"\n",
            "{\"auth_mode\":\"chatgpt\"}\n",
        );
        let provider = Z8ProviderConfig::default();
        let mut update = ProviderConfigUpdate::new(provider, "key-1", secret()).unwrap();
        update.expected_revision = Some(current.revision.clone());
        let plan = prepare_provider_config_update(&current, &update).unwrap();
        assert!(plan.config_toml.contains("model_provider = \"z8\""));
        assert!(plan.config_toml.contains("preferred_auth_method = \"apikey\""));
        assert!(plan.config_toml.contains("forced_login_method = \"api\""));
        assert!(plan.config_toml.contains(Z8_BASE_URL));
        assert!(!plan.config_toml.contains(secret().as_str()));
        assert!(plan.auth_json.contains(secret().as_str()));
        assert_ne!(plan.next_revision, plan.previous_revision);
        assert_eq!(plan.verify_against(&current), Ok(()));
        assert_eq!(plan.snapshot().revision, plan.next_revision);

        let transaction = ProviderConfigTransaction::begin(&current, &update).unwrap();
        assert_eq!(transaction.plan().previous_revision, current.revision);
        let committed = transaction.commit(&current).unwrap();
        assert_eq!(committed.revision, plan.next_revision);

        let stale = ProviderConfigSnapshot::new("model_provider = \"custom\"\n", "{}\n");
        let transaction = ProviderConfigTransaction::begin(&current, &update).unwrap();
        assert!(matches!(
            transaction.commit(&stale),
            Err(ProvisioningError::RevisionMismatch { .. })
        ));
    }

    #[test]
    fn config_update_preserves_the_existing_model_when_switching_keys() {
        let current = ProviderConfigSnapshot::new(
            "model_provider = \"z8\"\nmodel = \"gpt-5.4\"\n",
            "{\"auth_mode\":\"apikey\"}\n",
        );
        let update = ProviderConfigUpdate::new(
            Z8ProviderConfig::default(),
            "key-2",
            secret(),
        )
        .unwrap();

        let plan = prepare_provider_config_update(&current, &update).unwrap();

        assert!(plan.config_toml.contains("model = \"gpt-5.4\""));
        assert!(!plan.config_toml.contains("model = \"gpt-5.6-sol\""));
    }

    #[test]
    fn config_update_repoints_the_active_profile_provider() {
        let current = ProviderConfigSnapshot::new(
            r#"model_provider = "codex_local_access"
profile = "legacy"

[profiles.legacy]
model_provider = "codex_local_access"

[model_providers.codex_local_access]
base_url = "https://s.giiip.com"
"#,
            "{\"auth_mode\":\"apikey\"}\n",
        );
        let update = ProviderConfigUpdate::new(
            Z8ProviderConfig::default(),
            "key-profile",
            secret(),
        )
        .unwrap();

        assert!(!z8_provider_documents_are_applied(
            &current.config_toml,
            &current.auth_json,
            None
        ));
        let plan = prepare_provider_config_update(&current, &update).unwrap();
        assert!(plan.config_toml.contains("model_provider = \"z8\""));
        assert!(plan
            .config_toml
            .contains("[profiles.legacy]\nmodel_provider = \"z8\""));
        assert!(z8_provider_documents_are_applied(
            &plan.config_toml,
            &plan.auth_json,
            Some(secret().as_str())
        ));
    }

    #[test]
    fn invalid_auth_document_aborts_without_partial_plan() {
        let current = ProviderConfigSnapshot::new("model_provider = \"openai\"\n", "[]");
        let update =
            ProviderConfigUpdate::new(Z8ProviderConfig::default(), "key-1", secret()).unwrap();
        assert_eq!(
            prepare_provider_config_update(&current, &update),
            Err(ProvisioningError::InvalidConfig {
                document: "auth.json".to_string()
            })
        );
    }

    #[test]
    fn health_mapping_does_not_include_secret() {
        let provider = Z8ProviderConfig::default();
        let result = health_check_from_http(&provider, Z8_DEFAULT_MODEL, 401, Some(42));
        assert_eq!(result.status, HealthCheckStatus::KeyInvalid);
        assert_eq!(result.error.as_ref().unwrap().code, "provider_key_invalid");
        assert!(!serde_json::to_string(&result)
            .unwrap()
            .contains("z8_test_key"));

        let timeout = health_check_from_transport_error(
            &provider,
            Z8_DEFAULT_MODEL,
            HealthTransportError::Timeout,
        );
        assert_eq!(timeout.status, HealthCheckStatus::NetworkFailed);
        assert_eq!(timeout.error.unwrap().code, "provider_timeout");
    }
}
