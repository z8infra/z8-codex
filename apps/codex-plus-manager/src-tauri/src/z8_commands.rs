use std::fmt;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use codex_plus_core::relay_config::default_codex_home_dir;
use codex_plus_core::settings::{
    BackendSettings, RelayMode, RelayProfile, RelayProtocol, SettingsStore, atomic_write,
};
use codex_plus_core::z8_account::{
    AccountApiKey, AccountAuthSettings, AccountClient, AccountError, AccountOperation,
    AccountSession, AccountSessionStore, CancellationToken, LoginOutcome, LoginRequest,
    RedeemReceipt, RegisterRequest, VerifyCodeRequest,
};
use codex_plus_core::z8_provisioning::{
    ApiKeyStatus, ApiKeySummary, HealthCheckResult, ProviderConfigSnapshot,
    ProviderConfigTransaction, ProviderConfigUpdate, SecretApiKey, Z8_BASE_URL, Z8_DEFAULT_MODEL,
    Z8_DEFAULT_MODEL_LIST, Z8ProviderConfig, Z8_DEFAULT_PROFILE_CONFIG, health_check_from_http,
    health_check_from_transport_error, set_z8_api_key_in_auth_contents,
};
use codex_plus_core::z8_secure_store::{PersistedAccount, SecureStoreError, Z8SecureStore};
use serde::{Deserialize, Serialize};

use crate::account_challenges::{PendingLoginRegistry, PendingLoginStatus};
use crate::commands::CommandResult;

struct Z8State {
    sessions: AccountSessionStore,
    keys: Mutex<Vec<AccountApiKey>>,
}

static STATE: OnceLock<Z8State> = OnceLock::new();
static CONFIG_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static ACCOUNT_TRANSITION_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static PENDING_LOGIN: OnceLock<Mutex<PendingLoginRegistry>> = OnceLock::new();
static SECURE_STORE: Z8SecureStore = Z8SecureStore::new();

fn state() -> &'static Z8State {
    STATE.get_or_init(|| Z8State {
        sessions: AccountSessionStore::new(),
        keys: Mutex::new(Vec::new()),
    })
}

fn config_lock() -> &'static Mutex<()> {
    CONFIG_LOCK.get_or_init(|| Mutex::new(()))
}

fn account_transition_lock() -> &'static Mutex<()> {
    ACCOUNT_TRANSITION_LOCK.get_or_init(|| Mutex::new(()))
}

fn pending_login() -> &'static Mutex<PendingLoginRegistry> {
    PENDING_LOGIN.get_or_init(|| Mutex::new(PendingLoginRegistry::default()))
}

fn pending_status() -> PendingLoginStatus {
    pending_login()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .status(Instant::now())
}

fn cancel_pending_login() -> bool {
    pending_login()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .cancel()
}

fn secure_store() -> &'static Z8SecureStore {
    &SECURE_STORE
}

fn persist_account(
    session: &AccountSession,
    keys: &[AccountApiKey],
) -> Result<(), SecureStoreError> {
    secure_store().save(session, keys)
}

fn restore_persisted_account() -> Result<(), SecureStoreError> {
    // Startup restoration and account transitions must share one boundary. A
    // status request can arrive while a login/refresh is committing; without
    // this guard a stale vault snapshot could overwrite that newer session.
    let _guard = account_transition_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if state().sessions.snapshot().is_some() {
        return Ok(());
    }
    let Some(saved) = secure_store().load()? else {
        return Ok(());
    };
    state()
        .sessions
        .restore(saved.session)
        .map_err(|_| SecureStoreError::InvalidPayload)?;
    *state()
        .keys
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = saved.keys;
    Ok(())
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Z8AccountPayload {
    pub authenticated: bool,
    pub email: Option<String>,
    pub keys: Vec<ApiKeySummary>,
    pub pending_two_factor: bool,
    pub pending_email: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Z8AuthSettingsPayload {
    pub settings: AccountAuthSettings,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Z8VerificationPayload {
    pub email: Option<String>,
    pub sent: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Z8RedeemPayload {
    pub receipt: RedeemReceipt,
    pub account: Z8AccountPayload,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Z8LoginInput {
    pub email: String,
    pub password: String,
    #[serde(default)]
    pub turnstile_token: Option<String>,
    #[serde(default)]
    pub tencent_captcha_ticket: Option<String>,
    #[serde(default)]
    pub tencent_captcha_randstr: Option<String>,
}

impl fmt::Debug for Z8LoginInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Z8LoginInput")
            .field("email", &self.email)
            .field("password", &"[redacted]")
            .field(
                "turnstile_token",
                &self.turnstile_token.as_ref().map(|_| "[redacted]"),
            )
            .field(
                "tencent_captcha_ticket",
                &self.tencent_captcha_ticket.as_ref().map(|_| "[redacted]"),
            )
            .field(
                "tencent_captcha_randstr",
                &self.tencent_captcha_randstr.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Z8RegisterInput {
    pub email: String,
    pub password: String,
    #[serde(default)]
    pub verify_code: Option<String>,
    #[serde(default)]
    pub turnstile_token: Option<String>,
    #[serde(default)]
    pub tencent_captcha_ticket: Option<String>,
    #[serde(default)]
    pub tencent_captcha_randstr: Option<String>,
    #[serde(default)]
    pub promo_code: Option<String>,
    #[serde(default)]
    pub invitation_code: Option<String>,
    #[serde(default)]
    pub aff_code: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Z8VerificationInput {
    pub email: String,
    #[serde(default)]
    pub turnstile_token: Option<String>,
    #[serde(default)]
    pub tencent_captcha_ticket: Option<String>,
    #[serde(default)]
    pub tencent_captcha_randstr: Option<String>,
}

impl fmt::Debug for Z8VerificationInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Z8VerificationInput")
            .field("email", &self.email)
            .field(
                "turnstile_token",
                &self.turnstile_token.as_ref().map(|_| "[redacted]"),
            )
            .field(
                "tencent_captcha_ticket",
                &self.tencent_captcha_ticket.as_ref().map(|_| "[redacted]"),
            )
            .field(
                "tencent_captcha_randstr",
                &self.tencent_captcha_randstr.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Z8TwoFactorInput {
    pub code: String,
}

impl fmt::Debug for Z8TwoFactorInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Z8TwoFactorInput")
            .field("code", &"[redacted]")
            .finish()
    }
}

impl fmt::Debug for Z8RegisterInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Z8RegisterInput")
            .field("email", &self.email)
            .field("password", &"[redacted]")
            .field(
                "verify_code",
                &self.verify_code.as_ref().map(|_| "[redacted]"),
            )
            .field(
                "turnstile_token",
                &self.turnstile_token.as_ref().map(|_| "[redacted]"),
            )
            .field(
                "tencent_captcha_ticket",
                &self.tencent_captcha_ticket.as_ref().map(|_| "[redacted]"),
            )
            .field(
                "tencent_captcha_randstr",
                &self.tencent_captcha_randstr.as_ref().map(|_| "[redacted]"),
            )
            .field(
                "promo_code",
                &self.promo_code.as_ref().map(|_| "[redacted]"),
            )
            .field(
                "invitation_code",
                &self.invitation_code.as_ref().map(|_| "[redacted]"),
            )
            .field("aff_code", &self.aff_code.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

fn command_error<T: Serialize + Default>(error: impl Into<String>) -> CommandResult<T> {
    CommandResult {
        status: "failed".to_string(),
        message: error.into(),
        payload: T::default(),
    }
}

fn account_error<T: Serialize + Default>(error: AccountError) -> CommandResult<T> {
    command_error(error.to_string())
}

fn usage_error_message(code: &str) -> &'static str {
    match code {
        "usage_invalid_key" => "所选 API Key 无法读取用量，请刷新 Key 后重试",
        "usage_rate_limited" => "用量服务请求过于频繁，请稍后重试",
        "usage_timeout" => "用量服务响应超时，请稍后重试",
        "usage_unavailable" => "用量服务暂时不可用，请稍后重试",
        "usage_response_too_large" | "usage_invalid_response" => "用量服务返回的数据暂不可用",
        "usage_request_failed" => "用量接口请求失败，请稍后重试",
        "usage_credential_invalid" => "所选 API Key 无效，请刷新 Key 后重试",
        _ => "暂时无法读取账户用量，请稍后重试",
    }
}

fn is_z8_supplier_profile(profile: &RelayProfile) -> bool {
    let base_url = profile
        .upstream_base_url
        .trim()
        .trim_end_matches('/')
        .to_ascii_lowercase();
    let configured_url = profile
        .base_url
        .trim()
        .trim_end_matches('/')
        .to_ascii_lowercase();
    base_url == Z8_BASE_URL.trim_end_matches('/').to_ascii_lowercase()
        || configured_url == Z8_BASE_URL.trim_end_matches('/').to_ascii_lowercase()
        || profile.id.starts_with("z8")
        || profile.name.trim().eq_ignore_ascii_case("z8 中转")
}

fn configured_z8_provider() -> codex_plus_core::z8_provisioning::Z8ProviderConfig {
    let fallback = codex_plus_core::z8_provisioning::Z8ProviderConfig::default();
    let Ok(settings) = SettingsStore::default().load() else {
        return fallback;
    };
    let profile = settings.active_relay_profile();
    let base_url = codex_plus_core::relay_config::relay_profile_base_url(&profile);
    let model = codex_plus_core::relay_config::relay_profile_model(&profile);
    if base_url.trim().is_empty() || model.trim().is_empty() {
        return fallback;
    }
    codex_plus_core::z8_provisioning::Z8ProviderConfig {
        base_url,
        default_model: model.clone(),
        models: vec![model],
        ..fallback
    }
}

/// The supplier profile is the persisted source of truth for the selected
/// account key.  Reading its auth.json payload lets the account UI restore the
/// exact key after a refresh/restart without adding another secret-bearing
/// state file.
fn selected_supplier_key_id_from_settings(
    keys: &[AccountApiKey],
    settings: &BackendSettings,
) -> Option<String> {
    let profile = settings.active_relay_profile();
    if !is_z8_supplier_profile(&profile) {
        return None;
    }
    let selected_secret = codex_plus_core::relay_config::relay_profile_api_key(&profile);
    if selected_secret.trim().is_empty() {
        return None;
    }
    keys.iter()
        .find(|key| key.secret() == selected_secret && ApiKeyStatus::from(key.status.as_str()).is_usable())
        .map(|key| key.id.clone())
}

fn selected_supplier_key_id(keys: &[AccountApiKey]) -> Option<String> {
    let settings = SettingsStore::default().load().ok()?;
    selected_supplier_key_id_from_settings(keys, &settings)
}

fn account_payload(keys: &[AccountApiKey], session: Option<&AccountSession>) -> Z8AccountPayload {
    let selected_id = selected_supplier_key_id(keys);
    let summaries = keys
        .iter()
        .filter_map(|key| {
            let secret = SecretApiKey::new(key.secret()).ok()?;
            let mut summary = codex_plus_core::z8_provisioning::summarize_api_key(
                key.id.clone(),
                key.name.clone(),
                ApiKeyStatus::from(key.status.as_str()),
                &secret,
            );
            summary.created_at = key.created_at.clone();
            summary.expires_at = key.expires_at.clone();
            summary.selected = selected_id.as_deref() == Some(key.id.as_str());
            Some(summary)
        })
        .collect();
    let pending = pending_status();
    Z8AccountPayload {
        authenticated: session.is_some(),
        email: session.map(|value| value.user.email.clone()),
        keys: summaries,
        pending_two_factor: pending.pending_two_factor,
        pending_email: pending.pending_email,
    }
}

fn successful<T: Serialize>(message: impl Into<String>, payload: T) -> CommandResult<T> {
    CommandResult {
        status: "ok".to_string(),
        message: message.into(),
        payload,
    }
}

async fn fetch_keys(
    client: &AccountClient,
    session: &AccountSession,
    cancellation: &CancellationToken,
) -> Result<Vec<AccountApiKey>, AccountError> {
    client.list_api_keys(session, cancellation).await
}

/// Refresh and redemption can succeed even when the subsequent optional Key
/// listing is unavailable. Keep the current account's last known Keys rather
/// than persisting an empty list that would erase its usable Provider choice.
fn keys_after_optional_fetch(
    existing: &[AccountApiKey],
    fetched: Result<Vec<AccountApiKey>, AccountError>,
) -> (Vec<AccountApiKey>, bool) {
    match fetched {
        Ok(keys) => (keys, true),
        Err(_) => (existing.to_vec(), false),
    }
}

fn same_session(left: &AccountSession, right: &AccountSession) -> bool {
    left.access_token() == right.access_token()
        && left.refresh_token() == right.refresh_token()
        && left.user.email == right.user.email
}

fn set_keys(keys: Vec<AccountApiKey>) {
    *state()
        .keys
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = keys;
}

fn restore_persisted_snapshot(snapshot: Option<PersistedAccount>) -> Result<(), SecureStoreError> {
    match snapshot {
        Some(snapshot) => persist_account(&snapshot.session, &snapshot.keys),
        None => secure_store().clear(),
    }
}

/// Atomically publish a new session snapshot. Persistence happens before the
/// in-memory commit, so a failed credential-store write leaves the active
/// account untouched. If the generation was superseded while the write was in
/// progress, restore the previous vault snapshot as well.
fn persist_and_commit(
    operation: &AccountOperation,
    session: AccountSession,
    keys: &[AccountApiKey],
) -> Result<AccountSession, String> {
    let _guard = account_transition_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let previous = secure_store()
        .load()
        .map_err(|error| format!("无法读取现有 Z8 安全凭据：{error}"))?;
    persist_account(&session, keys).map_err(|error| format!("无法保存 Z8 安全凭据：{error}"))?;
    match operation.commit(session.clone()) {
        Ok(committed) => {
            set_keys(keys.to_vec());
            Ok(committed)
        }
        Err(error) => {
            let rollback = restore_persisted_snapshot(previous);
            if let Err(rollback_error) = rollback {
                return Err(format!(
                    "账户操作已过期，且无法恢复原有 Z8 安全凭据：{rollback_error}"
                ));
            }
            Err(error.to_string())
        }
    }
}

fn persist_current_keys(
    operation: &AccountOperation,
    session: &AccountSession,
    keys: &[AccountApiKey],
) -> Result<(), String> {
    let _guard = account_transition_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let previous = secure_store()
        .load()
        .map_err(|error| format!("无法读取现有 Z8 安全凭据：{error}"))?;
    let Some(current) = state().sessions.snapshot() else {
        return Err("Z8 账户已退出，请重试".to_string());
    };
    if !same_session(&current, session) {
        return Err("Z8 账户已切换，请重试".to_string());
    }
    persist_account(session, keys).map_err(|error| format!("无法保存 Z8 安全凭据：{error}"))?;
    // Reuse the operation generation gate for key-only refreshes. Comparing
    // token values is insufficient: a late request can belong to a newer
    // operation that happens to return the same session tokens.
    match operation.commit(session.clone()) {
        Ok(_) => {
            set_keys(keys.to_vec());
            Ok(())
        }
        Err(error) => {
            let rollback = restore_persisted_snapshot(previous);
            if let Err(rollback_error) = rollback {
                return Err(format!(
                    "账户已切换，且无法恢复原有 Z8 安全凭据：{rollback_error}"
                ));
            }
            Err(error.to_string())
        }
    }
}

#[tauri::command]
pub fn z8_account_status() -> CommandResult<Z8AccountPayload> {
    if let Err(error) = ensure_z8_supplier_profile_template() {
        return command_error(format!("无法初始化 Z8 供应商配置：{error}"));
    }
    if let Err(error) = restore_persisted_account() {
        return command_error(format!("无法读取 Z8 安全凭据：{error}"));
    }
    let current = state().sessions.snapshot();
    let keys = state()
        .keys
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    successful(
        "Z8 账户状态已加载",
        account_payload(&keys, current.as_ref()),
    )
}

#[tauri::command]
pub async fn z8_auth_settings() -> CommandResult<Z8AuthSettingsPayload> {
    let client = match AccountClient::default() {
        Ok(client) => client,
        Err(error) => return account_error(error),
    };
    match client.auth_settings(&CancellationToken::new()).await {
        Ok(settings) => successful("账户设置已加载", Z8AuthSettingsPayload { settings }),
        Err(error) => account_error(error),
    }
}

#[tauri::command]
pub async fn z8_send_verification_code(
    input: Z8VerificationInput,
) -> CommandResult<Z8VerificationPayload> {
    let client = match AccountClient::default() {
        Ok(client) => client,
        Err(error) => return account_error(error),
    };
    let request = VerifyCodeRequest {
        email: input.email.clone(),
        turnstile_token: input.turnstile_token,
        tencent_captcha_ticket: input.tencent_captcha_ticket,
        tencent_captcha_randstr: input.tencent_captcha_randstr,
    };
    match client
        .send_verification_code(&request, &CancellationToken::new())
        .await
    {
        Ok(()) => successful(
            "验证码已发送",
            Z8VerificationPayload {
                email: Some(input.email),
                sent: true,
            },
        ),
        Err(error) => account_error(error),
    }
}

#[tauri::command]
pub fn z8_cancel_login() -> CommandResult<Z8AccountPayload> {
    cancel_pending_login();
    let current = state().sessions.snapshot();
    let keys = state()
        .keys
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    successful("登录已取消", account_payload(&keys, current.as_ref()))
}

#[tauri::command]
pub async fn z8_complete_two_factor(input: Z8TwoFactorInput) -> CommandResult<Z8AccountPayload> {
    let handle = {
        let mut registry = pending_login()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        registry.handle(Instant::now())
    };
    let Some(handle) = handle else {
        return command_error("当前没有待完成的二次验证");
    };
    let client = match AccountClient::default() {
        Ok(client) => client,
        Err(error) => {
            pending_login()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .release(handle.id);
            return account_error(error);
        }
    };
    let cancellation = handle.operation.cancellation();
    let session = match client
        .complete_two_factor(&handle.challenge, &input.code, &cancellation)
        .await
    {
        Ok(session) => session,
        Err(error) => {
            pending_login()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .release(handle.id);
            return account_error(error);
        }
    };
    let keys = fetch_keys(&client, &session, &cancellation)
        .await
        .unwrap_or_default();
    match persist_and_commit(&handle.operation, session, &keys) {
        Ok(session) => {
            pending_login()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .finish(handle.id);
            successful("登录成功", account_payload(&keys, Some(&session)))
        }
        Err(error) => {
            pending_login()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .release(handle.id);
            command_error(format!("登录未完成：{error}"))
        }
    }
}

#[tauri::command]
pub async fn z8_login(input: Z8LoginInput) -> CommandResult<Z8AccountPayload> {
    cancel_pending_login();
    let client = match AccountClient::default() {
        Ok(client) => client,
        Err(error) => return account_error(error),
    };
    let operation = Arc::new(state().sessions.begin());
    let request = LoginRequest {
        email: input.email,
        password: input.password,
        turnstile_token: input.turnstile_token,
        tencent_captcha_ticket: input.tencent_captcha_ticket,
        tencent_captcha_randstr: input.tencent_captcha_randstr,
    };
    let cancellation = operation.cancellation();
    let outcome = match client.login_outcome(&request, &cancellation).await {
        Ok(outcome) => outcome,
        Err(error) => return account_error(error),
    };
    match outcome {
        LoginOutcome::TwoFactorRequired(challenge) => {
            let status = pending_login()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .begin(challenge, operation, Instant::now());
            let current = state().sessions.snapshot();
            let keys = state()
                .keys
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut payload = account_payload(&keys, current.as_ref());
            payload.pending_two_factor = status.pending_two_factor;
            payload.pending_email = status.pending_email;
            successful("请输入二次验证码", payload)
        }
        LoginOutcome::Authenticated(session) => {
            let keys = fetch_keys(&client, &session, &cancellation)
                .await
                .unwrap_or_default();
            match persist_and_commit(&operation, session, &keys) {
                Ok(session) => successful("登录成功", account_payload(&keys, Some(&session))),
                Err(error) => command_error(format!("登录未完成：{error}")),
            }
        }
    }
}

#[tauri::command]
pub async fn z8_register(input: Z8RegisterInput) -> CommandResult<Z8AccountPayload> {
    cancel_pending_login();
    let client = match AccountClient::default() {
        Ok(client) => client,
        Err(error) => return account_error(error),
    };
    let operation = state().sessions.begin();
    let request = RegisterRequest {
        email: input.email,
        password: input.password,
        verify_code: input.verify_code,
        turnstile_token: input.turnstile_token,
        tencent_captcha_ticket: input.tencent_captcha_ticket,
        tencent_captcha_randstr: input.tencent_captcha_randstr,
        promo_code: input.promo_code,
        invitation_code: input.invitation_code,
        aff_code: input.aff_code,
    };
    let cancellation = operation.cancellation();
    let session = match client.register(&request, &cancellation).await {
        Ok(session) => session,
        Err(error) => return account_error(error),
    };
    let keys = fetch_keys(&client, &session, &cancellation)
        .await
        .unwrap_or_default();
    match persist_and_commit(&operation, session, &keys) {
        Ok(session) => successful("注册成功", account_payload(&keys, Some(&session))),
        Err(error) => command_error(format!("注册未完成：{error}")),
    }
}

#[tauri::command]
pub async fn z8_refresh_session() -> CommandResult<Z8AccountPayload> {
    let Some(current) = state().sessions.snapshot() else {
        return command_error("请先登录 Z8 账户");
    };
    let existing_keys = state()
        .keys
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let client = match AccountClient::default() {
        Ok(client) => client,
        Err(error) => return account_error(error),
    };
    let operation = state().sessions.begin();
    let cancellation = operation.cancellation();
    let session = match client.refresh(&current, &cancellation).await {
        Ok(session) => session,
        Err(error) => return account_error(error),
    };
    let (keys, keys_updated) = keys_after_optional_fetch(
        &existing_keys,
        fetch_keys(&client, &session, &cancellation).await,
    );
    match persist_and_commit(&operation, session, &keys) {
        Ok(session) => successful(
            if keys_updated {
                "会话已刷新"
            } else {
                "会话已刷新，API Key 暂未更新，请稍后手动刷新"
            },
            account_payload(&keys, Some(&session)),
        ),
        Err(error) => command_error(format!("会话刷新未完成：{error}")),
    }
}

#[tauri::command]
pub async fn z8_redeem(code: String) -> CommandResult<Z8RedeemPayload> {
    let Some(current) = state().sessions.snapshot() else {
        return command_error("请先登录 Z8 账户");
    };
    let existing_keys = state()
        .keys
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let client = match AccountClient::default() {
        Ok(client) => client,
        Err(error) => return account_error(error),
    };
    let operation = state().sessions.begin();
    let cancellation = operation.cancellation();
    let receipt = match client.redeem(&current, &code, &cancellation).await {
        Ok(receipt) => receipt,
        Err(error) => return account_error(error),
    };
    let (keys, keys_updated) = keys_after_optional_fetch(
        &existing_keys,
        fetch_keys(&client, &current, &cancellation).await,
    );
    match persist_and_commit(&operation, current, &keys) {
        Ok(session) => successful(
            if keys_updated {
                "兑换成功"
            } else {
                "兑换成功，API Key 暂未更新，请稍后手动刷新"
            },
            Z8RedeemPayload {
                receipt,
                account: account_payload(&keys, Some(&session)),
            },
        ),
        Err(error) => command_error(format!("兑换未完成：{error}")),
    }
}

#[tauri::command]
pub async fn z8_logout() -> CommandResult<Z8AccountPayload> {
    let Some(current) = state().sessions.snapshot() else {
        let _guard = account_transition_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let clear_result = secure_store().clear();
        // A previous transition may have cleared the session already while
        // leaving an in-memory key snapshot behind. Always clear that cache
        // when there is no active session, even if native cleanup failed.
        set_keys(Vec::new());
        if let Err(error) = clear_result {
            return command_error(format!("退出登录失败，无法清理 Z8 安全凭据：{error}"));
        }
        return successful("已退出登录", account_payload(&[], None));
    };
    let client = match AccountClient::default() {
        Ok(client) => client,
        Err(error) => return account_error(error),
    };
    let operation = state().sessions.begin();
    let cancellation = operation.cancellation();
    if let Err(error) = client.logout(&current, &cancellation).await {
        return account_error(error);
    }
    let _guard = account_transition_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // A newer login/refresh may have replaced this operation while the
    // server logout request was in flight. Never clear that newer account's
    // vault or session.
    let Some(still_current) = state().sessions.snapshot() else {
        return successful("已退出登录", account_payload(&[], None));
    };
    if !same_session(&still_current, &current) {
        return command_error("账户已切换，请重试");
    }
    // Establish the secure-store logout tombstone and clear the native
    // snapshot before publishing the in-memory logout. The store writes the
    // tombstone first, so a partial delete remains fail-closed across a
    // restart. If the marker cannot be established, keep the active session
    // visible and report failure rather than making a logout claim that could
    // resurrect an old record later.
    if let Err(error) = secure_store().clear() {
        return command_error(format!(
            "退出登录未完成，无法安全清理 Z8 凭据，请重试：{error}"
        ));
    }
    // The request operation may have been superseded by a transient login or
    // refresh that never committed. We already verified the active session
    // under the transition lock, so publish logout through a fresh generation.
    let logout_result = state().sessions.begin().logout();
    if logout_result.is_ok() {
        // Do not leave API keys in memory after a successful server logout.
        set_keys(Vec::new());
    }
    if let Err(error) = logout_result {
        return account_error(error);
    }
    successful("已退出登录", account_payload(&[], None))
}

#[tauri::command]
pub async fn z8_refresh_keys() -> CommandResult<Z8AccountPayload> {
    let Some(session) = state().sessions.snapshot() else {
        return command_error("请先登录 Z8 账户");
    };
    let client = match AccountClient::default() {
        Ok(client) => client,
        Err(error) => return account_error(error),
    };
    let operation = state().sessions.begin();
    if state()
        .sessions
        .snapshot()
        .as_ref()
        .is_none_or(|value| !same_session(value, &session))
    {
        return command_error("Z8 账户已切换，请重试");
    }
    let cancellation = operation.cancellation();
    let keys = match fetch_keys(&client, &session, &cancellation).await {
        Ok(keys) => keys,
        Err(error) => return account_error(error),
    };
    if let Err(error) = persist_current_keys(&operation, &session, &keys) {
        return command_error(format!("API Key 刷新未完成：{error}"));
    }
    successful("API Key 已刷新", account_payload(&keys, Some(&session)))
}

fn read_document(path: &Path, fallback: &str) -> anyhow::Result<(String, Option<Vec<u8>>)> {
    match fs::read(path) {
        Ok(bytes) => Ok((String::from_utf8(bytes.clone())?, Some(bytes))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok((fallback.to_string(), None))
        }
        Err(error) => Err(error.into()),
    }
}

fn restore_document(path: &Path, previous: Option<&[u8]>) -> anyhow::Result<()> {
    match previous {
        Some(bytes) => atomic_write(path, bytes),
        None => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        },
    }
}

fn provider_write_rollback_error() -> anyhow::Error {
    anyhow::anyhow!("无法写入 Provider 配置，且回滚失败")
}

fn prepare_z8_supplier_profile(
    settings: &mut BackendSettings,
    key: &AccountApiKey,
) -> anyhow::Result<String> {
    let profile_index = settings
        .relay_profiles
        .iter()
        .position(is_z8_supplier_profile)
        .unwrap_or_else(|| {
            settings
                .relay_profiles
                .push(new_z8_supplier_profile(settings));
            settings.relay_profiles.len() - 1
        });
    let is_new = settings
        .relay_profiles
        .get(profile_index)
        .is_some_and(|profile| profile.config_contents.trim().is_empty());
    let profile = settings
        .relay_profiles
        .get_mut(profile_index)
        .ok_or_else(|| anyhow::anyhow!("无法创建 Z8 供应商配置"))?;
    if is_new {
        initialize_z8_supplier_profile(profile, Some(key))?;
    } else {
        update_z8_supplier_key(profile, key)?;
    }
    settings.relay_profiles_enabled = true;
    settings.active_relay_id = profile.id.clone();
    Ok(profile.id.clone())
}

fn new_z8_supplier_profile(settings: &BackendSettings) -> RelayProfile {
    let mut profile = RelayProfile::default();
    let mut id = "z8-launch".to_string();
    let mut suffix = 2u32;
    while settings.relay_profiles.iter().any(|item| item.id == id) {
        id = format!("z8-launch-{suffix}");
        suffix += 1;
    }
    profile.id = id;
    profile.name = "Z8 中转".to_string();
    profile
}

fn initialize_z8_supplier_profile(
    profile: &mut RelayProfile,
    key: Option<&AccountApiKey>,
) -> anyhow::Result<()> {
    profile.name = "Z8 中转".to_string();
    profile.model = Z8_DEFAULT_MODEL.to_string();
    profile.model_list = Z8_DEFAULT_MODEL_LIST.to_string();
    profile.base_url = Z8_BASE_URL.to_string();
    profile.upstream_base_url = Z8_BASE_URL.to_string();
    profile.protocol = RelayProtocol::Responses;
    profile.relay_mode = RelayMode::PureApi;
    profile.official_mix_api_key = false;
    profile.no_auth = false;
    profile.config_contents = Z8_DEFAULT_PROFILE_CONFIG.to_string();
    profile.auth_contents = String::new();
    profile.use_common_config = true;
    if let Some(key) = key {
        update_z8_supplier_key(profile, key)?;
    }
    codex_plus_core::relay_config::normalize_relay_profile_for_storage(profile)?;
    Ok(())
}

fn update_z8_supplier_key(profile: &mut RelayProfile, key: &AccountApiKey) -> anyhow::Result<()> {
    profile.api_key = key.secret().to_string();
    profile.auth_contents = set_z8_api_key_in_auth_contents(&profile.auth_contents, key.secret())
        .or_else(|_| set_z8_api_key_in_auth_contents("", key.secret()))?;
    Ok(())
}

fn ensure_z8_supplier_profile_template() -> anyhow::Result<()> {
    let store = SettingsStore::default();
    let mut settings = store.load()?;
    if settings.relay_profiles.iter().any(is_z8_supplier_profile) {
        return Ok(());
    }
    let mut profile = new_z8_supplier_profile(&settings);
    initialize_z8_supplier_profile(&mut profile, None)?;
    settings.relay_profiles.push(profile);
    settings.relay_profiles_enabled = true;
    store.save(&settings)
}

fn reset_z8_supplier_profile(
    settings: &mut BackendSettings,
    key: Option<&AccountApiKey>,
) -> anyhow::Result<String> {
    let profile_index = settings
        .relay_profiles
        .iter()
        .position(is_z8_supplier_profile)
        .unwrap_or_else(|| {
            settings
                .relay_profiles
                .push(new_z8_supplier_profile(settings));
            settings.relay_profiles.len() - 1
        });
    let profile = settings
        .relay_profiles
        .get_mut(profile_index)
        .ok_or_else(|| anyhow::anyhow!("无法创建 Z8 供应商配置"))?;
    initialize_z8_supplier_profile(profile, key)?;
    settings.relay_profiles_enabled = true;
    if key.is_some() {
        settings.active_relay_id = profile.id.clone();
    }
    Ok(profile.id.clone())
}

/// Apply the same profile that the supplier list displays.  This is kept as
/// one transaction so settings.json and Codex's live files cannot disagree.
fn apply_z8_supplier_profile(key: &AccountApiKey) -> anyhow::Result<()> {
    let store = SettingsStore::default();
    let previous = store.load()?;
    let mut next = previous.clone();
    prepare_z8_supplier_profile(&mut next, key)?;
    let home = default_codex_home_dir();
    codex_plus_core::relay_switch::switch_relay_profile_in_home(
        &store,
        &home,
        next,
        &previous.active_relay_id,
    )?;
    Ok(())
}

fn apply_provider_key(key: &AccountApiKey) -> anyhow::Result<()> {
    let _guard = config_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = default_codex_home_dir();
    fs::create_dir_all(&home)?;
    let config_path = home.join("config.toml");
    let auth_path = home.join("auth.json");
    let (config, old_config) = read_document(&config_path, "model_provider = \"z8\"\n")?;
    let (auth, old_auth) = read_document(&auth_path, "{}\n")?;
    let current = ProviderConfigSnapshot::new(config, auth);
    let secret =
        SecretApiKey::new(key.secret()).map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let update = ProviderConfigUpdate::new(Z8ProviderConfig::default(), key.id.clone(), secret)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let transaction = ProviderConfigTransaction::begin(&current, &update)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let plan = transaction
        .commit(&current)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    atomic_write(&auth_path, plan.auth_json.as_bytes())?;
    if let Err(error) = atomic_write(&config_path, plan.config_toml.as_bytes()) {
        let auth_restore = restore_document(&auth_path, old_auth.as_deref());
        let config_restore = restore_document(&config_path, old_config.as_deref());
        if auth_restore.is_err() || config_restore.is_err() {
            return Err(provider_write_rollback_error());
        }
        return Err(error);
    }
    Ok(())
}

/// Re-check the live Codex documents immediately before a Z8 launch. The
/// renderer's selected-key state is advisory; another Codex/relay operation
/// may have changed the files after the account page applied them. When the
/// documents are stale, repair them from the native secure-store snapshot
/// without returning the secret across the Tauri boundary.
pub fn ensure_z8_provider_applied_for_launch() -> anyhow::Result<()> {
    let saved = secure_store()
        .load()
        .map_err(|error| anyhow::anyhow!("读取 Z8 安全凭据失败：{error}"))?
        .ok_or_else(|| anyhow::anyhow!("请先登录 Z8 账户并选择 API Key"))?;
    let selected_id = selected_supplier_key_id(&saved.keys);
    let key = selected_id
        .as_deref()
        .and_then(|id| saved.keys.iter().find(|key| key.id == id))
        .or_else(|| {
            saved
                .keys
                .iter()
                .find(|key| ApiKeyStatus::from(key.status.as_str()).is_usable())
        })
        .ok_or_else(|| anyhow::anyhow!("当前 Z8 账户没有可用 API Key"))?;
    apply_z8_supplier_profile(key)
        .map_err(|error| anyhow::anyhow!("启动前同步 Z8 供应商配置失败：{error}"))
}

#[tauri::command]
pub async fn z8_apply_key(key_id: String) -> CommandResult<Z8AccountPayload> {
    let Some(session) = state().sessions.snapshot() else {
        return command_error("请先登录 Z8 账户");
    };
    let client = match AccountClient::default() {
        Ok(client) => client,
        Err(error) => return account_error(error),
    };
    let operation = state().sessions.begin();
    if state()
        .sessions
        .snapshot()
        .as_ref()
        .is_none_or(|value| !same_session(value, &session))
    {
        return command_error("Z8 账户已切换，请重试");
    }
    let cancellation = operation.cancellation();
    let keys = match fetch_keys(&client, &session, &cancellation).await {
        Ok(keys) => keys,
        Err(error) => return account_error(error),
    };
    if let Err(error) = persist_current_keys(&operation, &session, &keys) {
        return command_error(format!("无法更新 Z8 安全凭据：{error}"));
    }
    let Some(key) = keys.iter().find(|value| value.id == key_id) else {
        return CommandResult {
            status: "failed".to_string(),
            message: "找不到所选 API Key".to_string(),
            payload: account_payload(&keys, Some(&session)),
        };
    };
    if !matches!(
        ApiKeyStatus::from(key.status.as_str()),
        ApiKeyStatus::Active
    ) {
        return command_error("所选 API Key 当前不可用");
    }
    if let Err(error) = apply_z8_supplier_profile(key) {
        return command_error(format!("写入 Z8 供应商配置失败：{error}"));
    }
    successful("Z8 Provider 已配置", account_payload(&keys, Some(&session)))
}

#[tauri::command]
pub fn z8_reset_supplier_profile() -> CommandResult<Z8AccountPayload> {
    let store = SettingsStore::default();
    let previous = match store.load() {
        Ok(settings) => settings,
        Err(error) => return command_error(format!("读取供应商配置失败：{error}")),
    };
    let saved = match secure_store().load() {
        Ok(saved) => saved,
        Err(error) => return command_error(format!("读取 Z8 安全凭据失败：{error}")),
    };
    let key = saved.as_ref().and_then(|account| {
        let selected_id = selected_supplier_key_id_from_settings(&account.keys, &previous);
        selected_id
            .as_deref()
            .and_then(|id| account.keys.iter().find(|key| key.id == id))
            .or_else(|| {
                account
                    .keys
                    .iter()
                    .find(|key| ApiKeyStatus::from(key.status.as_str()).is_usable())
            })
    });
    let mut next = previous.clone();
    if let Err(error) = reset_z8_supplier_profile(&mut next, key) {
        return command_error(format!("恢复 Z8 默认配置失败：{error}"));
    }
    let home = default_codex_home_dir();
    let result = if key.is_some() {
        codex_plus_core::relay_switch::switch_relay_profile_in_home(
            &store,
            &home,
            next,
            &previous.active_relay_id,
        )
        .map(|_| ())
    } else {
        store.save(&next)
    };
    if let Err(error) = result {
        return command_error(format!("恢复 Z8 默认配置失败：{error}"));
    }
    let current = state().sessions.snapshot();
    let keys = state()
        .keys
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    successful("Z8 默认供应商配置已恢复", account_payload(&keys, current.as_ref()))
}

#[tauri::command]
pub async fn z8_usage(key_id: String) -> CommandResult<codex_plus_core::z8_usage::UsageSnapshot> {
    let Some(session) = state().sessions.snapshot() else {
        return command_error("请先登录 Z8 账户");
    };
    let key = {
        let keys = state()
            .keys
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        keys.iter().find(|key| key.id == key_id).cloned()
    };
    let Some(key) = key else {
        return command_error("找不到所选 API Key，请刷新 Key 后重试");
    };
    if !matches!(
        ApiKeyStatus::from(key.status.as_str()),
        ApiKeyStatus::Active
    ) {
        return command_error("所选 API Key 当前不可用");
    }
    let result = codex_plus_core::z8_usage::fetch_with_api_key(key.secret().to_string()).await;
    if state()
        .sessions
        .snapshot()
        .as_ref()
        .is_none_or(|current| !same_session(current, &session))
    {
        return command_error("Z8 账户已切换，请重新选择 API Key");
    }
    match result {
        Ok(snapshot) => successful("账户用量已刷新", snapshot),
        Err(error) => command_error(usage_error_message(error)),
    }
}

#[tauri::command]
pub async fn z8_check_provider(key_id: String) -> CommandResult<HealthCheckResult> {
    let provider = configured_z8_provider();
    let Some(session) = state().sessions.snapshot() else {
        return CommandResult {
            status: "failed".to_string(),
            message: "请先登录 Z8 账户".to_string(),
            payload: health_check_from_transport_error(
                &provider,
                &provider.default_model,
                codex_plus_core::z8_provisioning::HealthTransportError::Network,
            ),
        };
    };
    let client = match AccountClient::default() {
        Ok(client) => client,
        Err(error) => {
            return CommandResult {
                status: "failed".to_string(),
                message: error.to_string(),
                payload: health_check_from_transport_error(
                    &provider,
                    &provider.default_model,
                    codex_plus_core::z8_provisioning::HealthTransportError::Network,
                ),
            };
        }
    };
    let keys = match fetch_keys(&client, &session, &CancellationToken::new()).await {
        Ok(keys) => keys,
        Err(error) => {
            return CommandResult {
                status: "failed".to_string(),
                message: error.to_string(),
                payload: health_check_from_transport_error(
                    &provider,
                    &provider.default_model,
                    codex_plus_core::z8_provisioning::HealthTransportError::Network,
                ),
            };
        }
    };
    let Some(key) = keys.iter().find(|value| value.id == key_id) else {
        return CommandResult {
            status: "failed".to_string(),
            message: "找不到所选 API Key".to_string(),
            payload: health_check_from_transport_error(
                &provider,
                &provider.default_model,
                codex_plus_core::z8_provisioning::HealthTransportError::Network,
            ),
        };
    };
    let started = std::time::Instant::now();
    let response = reqwest::Client::new()
        .get(provider.health_endpoint())
        .bearer_auth(key.secret())
        .timeout(std::time::Duration::from_secs(12))
        .send()
        .await;
    let result = match response {
        Ok(response) => health_check_from_http(
            &provider,
            &provider.default_model,
            response.status().as_u16(),
            Some(started.elapsed().as_millis() as u64),
        ),
        Err(error) => health_check_from_transport_error(
            &provider,
            &provider.default_model,
            if error.is_timeout() {
                codex_plus_core::z8_provisioning::HealthTransportError::Timeout
            } else {
                codex_plus_core::z8_provisioning::HealthTransportError::Network
            },
        ),
    };
    let status = if matches!(
        result.status,
        codex_plus_core::z8_provisioning::HealthCheckStatus::Healthy
    ) {
        "ok"
    } else {
        "failed"
    };
    CommandResult {
        status: status.to_string(),
        message: result
            .error
            .as_ref()
            .map(|error| error.message.clone())
            .unwrap_or_else(|| "Z8 Provider 连接正常".to_string()),
        payload: result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_api_key(id: i64, secret: &str) -> AccountApiKey {
        AccountApiKey::from_persisted_value(&serde_json::json!({
            "id": id,
            "name": "测试 Key",
            "status": "active",
            "key": secret,
        }))
        .expect("valid test API Key")
    }

    #[test]
    fn optional_key_fetch_failure_preserves_existing_account_keys() {
        let existing = vec![test_api_key(7, "z8-existing-secret")];
        let error = AccountClient::new("not-a-valid-url")
            .err()
            .expect("invalid URL should produce a stable error");
        let (keys, updated) = keys_after_optional_fetch(&existing, Err(error));
        assert!(!updated);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].id, "7");
        assert_eq!(keys[0].secret(), "z8-existing-secret");
    }

    #[test]
    fn successful_empty_key_listing_clears_stale_keys() {
        let existing = vec![test_api_key(7, "z8-existing-secret")];
        let (keys, updated) = keys_after_optional_fetch(&existing, Ok(Vec::new()));
        assert!(updated);
        assert!(keys.is_empty());
    }

    #[test]
    fn selecting_a_key_updates_the_z8_supplier_profile_and_selection_snapshot() {
        let key = test_api_key(17, "z8-selected-secret");
        let mut settings = BackendSettings::default();
        let original_config = "model = \"custom-model\"\nmodel_provider = \"custom\"\n\n[features]\ngoals = false\n\n[model_providers.custom]\nname = \"my relay\"\nwire_api = \"responses\"\nbase_url = \"https://custom.example/v1\"\n";
        settings.relay_profiles = vec![RelayProfile {
            id: "z8-launch-2".to_string(),
            name: "Z8 中转".to_string(),
            base_url: "https://custom.example/v1".to_string(),
            upstream_base_url: "https://custom.example/v1".to_string(),
            model: "gpt-5.6-sol".to_string(),
            auth_contents: r#"{"OPENAI_API_KEY":"old-secret"}"#.to_string(),
            config_contents: original_config.to_string(),
            protocol: RelayProtocol::Responses,
            relay_mode: RelayMode::PureApi,
            ..RelayProfile::default()
        }];
        settings.active_relay_id = "z8-launch-2".to_string();

        prepare_z8_supplier_profile(&mut settings, &key).expect("profile should be prepared");

        let profile = settings.active_relay_profile();
        assert_eq!(profile.base_url, "https://custom.example/v1");
        assert_eq!(profile.upstream_base_url, "https://custom.example/v1");
        assert_eq!(profile.relay_mode, RelayMode::PureApi);
        assert_eq!(profile.model, "gpt-5.6-sol");
        assert_eq!(profile.config_contents, original_config);
        assert!(profile.auth_contents.contains("z8-selected-secret"));
        assert_eq!(
            selected_supplier_key_id_from_settings(&[key], &settings),
            Some("17".to_string())
        );
    }

    #[test]
    fn first_z8_profile_uses_the_default_template() {
        let key = test_api_key(18, "z8-first-secret");
        let mut settings = BackendSettings::default();

        prepare_z8_supplier_profile(&mut settings, &key).expect("profile should be created");

        let profile = settings.active_relay_profile();
        assert_eq!(profile.model, Z8_DEFAULT_MODEL);
        assert_eq!(
            profile.model_list.lines().collect::<std::collections::HashSet<_>>(),
            Z8_DEFAULT_MODEL_LIST.lines().collect::<std::collections::HashSet<_>>()
        );
        assert_eq!(profile.base_url, Z8_BASE_URL);
        assert_eq!(profile.protocol, RelayProtocol::Responses);
        assert_eq!(profile.relay_mode, RelayMode::PureApi);
        assert!(profile.config_contents.contains("model = \"gpt-6-astra\""));
        assert!(profile.config_contents.contains("goals = true"));
        assert!(profile.auth_contents.contains("z8-first-secret"));
    }

    #[test]
    fn reset_z8_profile_restores_defaults_but_keeps_selected_key() {
        let key = test_api_key(19, "z8-reset-secret");
        let mut settings = BackendSettings::default();
        let mut profile = new_z8_supplier_profile(&settings);
        profile.model = "user-model".to_string();
        profile.base_url = "https://user.example".to_string();
        profile.config_contents = "model = \"user-model\"\n".to_string();
        profile.auth_contents = r#"{"OPENAI_API_KEY":"old-secret","auth_mode":"apikey"}"#.to_string();
        settings.relay_profiles.push(profile);

        reset_z8_supplier_profile(&mut settings, Some(&key)).expect("profile should reset");

        let profile = settings.active_relay_profile();
        assert_eq!(profile.model, Z8_DEFAULT_MODEL);
        assert_eq!(
            profile.model_list.lines().collect::<std::collections::HashSet<_>>(),
            Z8_DEFAULT_MODEL_LIST.lines().collect::<std::collections::HashSet<_>>()
        );
        assert_eq!(profile.base_url, Z8_BASE_URL);
        assert!(profile.config_contents.contains("model_provider = \"custom\""));
        assert!(profile.auth_contents.contains("z8-reset-secret"));
        assert!(!profile.auth_contents.contains("old-secret"));
    }

    #[test]
    fn account_command_debug_redacts_all_credential_fields() {
        let login = Z8LoginInput {
            email: "user@example.com".to_string(),
            password: "password-secret".to_string(),
            turnstile_token: Some("turnstile-secret".to_string()),
            tencent_captcha_ticket: Some("ticket-secret".to_string()),
            tencent_captcha_randstr: Some("randstr-secret".to_string()),
        };
        let register = Z8RegisterInput {
            email: "user@example.com".to_string(),
            password: "password-secret".to_string(),
            verify_code: Some("verify-secret".to_string()),
            turnstile_token: Some("turnstile-secret".to_string()),
            tencent_captcha_ticket: Some("ticket-secret".to_string()),
            tencent_captcha_randstr: Some("randstr-secret".to_string()),
            promo_code: Some("promo-secret".to_string()),
            invitation_code: Some("invite-secret".to_string()),
            aff_code: Some("affiliate-secret".to_string()),
        };
        let verification = Z8VerificationInput {
            email: "user@example.com".to_string(),
            turnstile_token: Some("turnstile-secret".to_string()),
            tencent_captcha_ticket: Some("ticket-secret".to_string()),
            tencent_captcha_randstr: Some("randstr-secret".to_string()),
        };
        let two_factor = Z8TwoFactorInput {
            code: "totp-secret".to_string(),
        };

        for debug in [
            format!("{login:?}"),
            format!("{register:?}"),
            format!("{verification:?}"),
            format!("{two_factor:?}"),
        ] {
            for secret in [
                "password-secret",
                "turnstile-secret",
                "ticket-secret",
                "randstr-secret",
                "verify-secret",
                "promo-secret",
                "invite-secret",
                "affiliate-secret",
                "totp-secret",
            ] {
                assert!(!debug.contains(secret), "debug output leaked {secret}");
            }
        }
    }

    #[test]
    fn restore_document_restores_bytes_and_treats_missing_delete_as_success() {
        let path =
            std::env::temp_dir().join(format!("z8-codex-restore-test-{}", std::process::id()));
        let _ = fs::remove_file(&path);
        restore_document(&path, None).expect("missing file is already restored");
        restore_document(&path, Some(b"old contents")).expect("restore should write bytes");
        assert_eq!(fs::read(&path).unwrap(), b"old contents");
        restore_document(&path, None).expect("restore should remove new file");
        assert!(!path.exists());
    }

    #[test]
    fn rollback_failure_message_is_stable_and_contains_no_credentials() {
        let message = provider_write_rollback_error().to_string();
        assert_eq!(message, "无法写入 Provider 配置，且回滚失败");
        assert!(!message.contains("api-key"));
        assert!(!message.contains("token"));
    }

    #[test]
    fn usage_error_messages_are_stable_and_never_echo_server_details() {
        assert_eq!(
            usage_error_message("usage_invalid_key"),
            "所选 API Key 无法读取用量，请刷新 Key 后重试"
        );
        assert_eq!(
            usage_error_message("unexpected-server-body"),
            "暂时无法读取账户用量，请稍后重试"
        );
    }

    #[test]
    fn usage_command_status_is_not_shadowed_by_account_status() {
        let snapshot = codex_plus_core::z8_usage::UsageSnapshot {
            account_status: Some("active".to_string()),
            ..Default::default()
        };
        let response = successful("账户用量已刷新", snapshot);
        let json = serde_json::to_value(response).expect("command response should serialize");

        // CommandResult flattens its payload for the renderer.  The usage
        // service's status is therefore named accountStatus so it cannot
        // overwrite the command envelope's success marker.
        assert_eq!(json.get("status").and_then(serde_json::Value::as_str), Some("ok"));
        assert_eq!(
            json.get("accountStatus").and_then(serde_json::Value::as_str),
            Some("active")
        );
        assert_eq!(
            json.get("message").and_then(serde_json::Value::as_str),
            Some("账户用量已刷新")
        );
    }
}
