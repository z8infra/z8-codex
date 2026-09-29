//! Z8 account API and session state.
//!
//! This module contains the account behaviour that is shared by the Z8
//! branded Codex++ manager.  It deliberately does not know about Tauri,
//! desktop launchers, or configuration files.  A caller can therefore test
//! it with a mock HTTP server and decide where a committed session is stored.
//!
//! The API shape follows the Z8 Launch account endpoints, while the state
//! handling is intentionally smaller: an in-flight request can never replace
//! the current session after it has been cancelled or superseded.

use reqwest::{Method, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

/// The account API origin used by the Z8 production service.
pub const DEFAULT_Z8_ACCOUNT_BASE_URL: &str = "https://z8.hk/v1";
const MAX_RESPONSE_BYTES: usize = 512 * 1024;
const MAX_TOKEN_BYTES: usize = 16 * 1024;
const MAX_EMAIL_BYTES: usize = 320;
const MAX_CODE_BYTES: usize = 512;

/// Stable account failure categories exposed to the UI layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountErrorCode {
    InvalidInput,
    InvalidCredentials,
    RegistrationDisabled,
    RegistrationInvalid,
    EmailExists,
    CaptchaFailed,
    CaptchaUnavailable,
    VerificationFailed,
    EmailReserved,
    EmailVerifyRequired,
    EmailSuffixNotAllowed,
    EmailDomainLimit,
    InvitationRequired,
    InvitationInvalid,
    BackendAdminOnly,
    RegistrationDefaultGroupUnavailable,
    RegistrationUnavailable,
    RegistrationApiKeyProvisionFailed,
    TwoFactorRequired,
    TwoFactorInvalid,
    SecurityChallenge,
    SessionExpired,
    SessionRevoked,
    Forbidden,
    Conflict,
    RateLimited,
    Timeout,
    Unavailable,
    InvalidResponse,
    RequestFailed,
    Cancelled,
}

impl AccountErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "account_request_invalid",
            Self::InvalidCredentials => "account_invalid_credentials",
            Self::RegistrationDisabled => "account_registration_disabled",
            Self::RegistrationInvalid => "account_registration_invalid",
            Self::EmailExists => "account_email_exists",
            Self::CaptchaFailed => "account_captcha_failed",
            Self::CaptchaUnavailable => "account_captcha_unavailable",
            Self::VerificationFailed => "account_verification_failed",
            Self::EmailReserved => "account_email_reserved",
            Self::EmailVerifyRequired => "account_email_verify_required",
            Self::EmailSuffixNotAllowed => "account_email_suffix_not_allowed",
            Self::EmailDomainLimit => "account_email_domain_limit",
            Self::InvitationRequired => "account_invitation_required",
            Self::InvitationInvalid => "account_invitation_invalid",
            Self::BackendAdminOnly => "account_backend_admin_only",
            Self::RegistrationDefaultGroupUnavailable => {
                "account_registration_default_group_unavailable"
            }
            Self::RegistrationUnavailable => "account_registration_unavailable",
            Self::RegistrationApiKeyProvisionFailed => {
                "account_registration_api_key_provision_failed"
            }
            Self::TwoFactorRequired => "account_two_factor_required",
            Self::TwoFactorInvalid => "account_two_factor_invalid",
            Self::SecurityChallenge => "account_security_challenge",
            Self::SessionExpired => "account_session_expired",
            Self::SessionRevoked => "account_session_revoked",
            Self::Forbidden => "account_forbidden",
            Self::Conflict => "account_conflict",
            Self::RateLimited => "account_rate_limited",
            Self::Timeout => "account_timeout",
            Self::Unavailable => "account_unavailable",
            Self::InvalidResponse => "account_invalid_response",
            Self::RequestFailed => "account_request_failed",
            Self::Cancelled => "account_login_cancelled",
        }
    }
}

/// A non-sensitive, stable error returned to the UI layer.
#[derive(Clone, PartialEq, Eq)]
pub struct AccountError {
    code: AccountErrorCode,
    message: String,
    retry_after_seconds: Option<u64>,
}

impl AccountError {
    fn new(code: AccountErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retry_after_seconds: None,
        }
    }

    fn with_retry_after(mut self, retry_after_seconds: Option<u64>) -> Self {
        self.retry_after_seconds = retry_after_seconds;
        self
    }

    pub fn code(&self) -> AccountErrorCode {
        self.code
    }

    pub fn stable_code(&self) -> &'static str {
        self.code.as_str()
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn retry_after_seconds(&self) -> Option<u64> {
        self.retry_after_seconds
    }
}

impl fmt::Debug for AccountError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AccountError")
            .field("code", &self.code)
            .field("message", &self.message)
            .field("retry_after_seconds", &self.retry_after_seconds)
            .finish()
    }
}

impl fmt::Display for AccountError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.stable_code(), self.message)
    }
}

impl std::error::Error for AccountError {}

/// An explicit cancellation handle for account requests.
#[derive(Clone, Default)]
pub struct CancellationToken {
    inner: Arc<CancellationState>,
}

#[derive(Default)]
struct CancellationState {
    cancelled: AtomicBool,
    notify: Notify,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        if !self.inner.cancelled.swap(true, Ordering::AcqRel) {
            self.inner.notify.notify_waiters();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }

    async fn cancelled(&self) {
        let notified = self.inner.notify.notified();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }
}

/// Login payload accepted by the Z8 account endpoint.
#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turnstile_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tencent_captcha_ticket: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tencent_captcha_randstr: Option<String>,
}

impl fmt::Debug for LoginRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoginRequest")
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

impl LoginRequest {
    pub fn new(email: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            email: email.into(),
            password: password.into(),
            turnstile_token: None,
            tencent_captcha_ticket: None,
            tencent_captcha_randstr: None,
        }
    }
}

/// Registration payload accepted by the Z8 account endpoint.
#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turnstile_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tencent_captcha_ticket: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tencent_captcha_randstr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promo_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invitation_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aff_code: Option<String>,
}

impl fmt::Debug for RegisterRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegisterRequest")
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

impl RegisterRequest {
    pub fn new(email: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            email: email.into(),
            password: password.into(),
            verify_code: None,
            turnstile_token: None,
            tencent_captcha_ticket: None,
            tencent_captcha_randstr: None,
            promo_code: None,
            invitation_code: None,
            aff_code: None,
        }
    }
}

/// Public account settings returned by `GET /api/v1/settings/public`.
///
/// Only values intended for a client are represented here.  In particular,
/// server-side captcha secrets are deliberately ignored by the decoder.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountAuthSettings {
    pub registration_enabled: bool,
    pub email_verify_enabled: bool,
    pub invitation_code_enabled: bool,
    pub promo_code_enabled: bool,
    pub turnstile_enabled: bool,
    pub turnstile_site_key: Option<String>,
    pub tencent_captcha_enabled: bool,
    pub tencent_captcha_app_id: Option<String>,
    pub tencent_captcha_region: Option<String>,
    pub aliyun_captcha_enabled: bool,
    pub aliyun_captcha_scene_id: Option<String>,
    pub aliyun_captcha_prefix: Option<String>,
    pub aliyun_captcha_region: Option<String>,
    pub login_agreement_enabled: bool,
    pub login_agreement_mode: Option<String>,
    pub login_agreement_revision: Option<String>,
    pub login_agreement_updated_at: Option<String>,
    pub login_agreement_documents: Vec<LoginAgreementDocument>,
}

/// A public legal document displayed before Z8 account login or registration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginAgreementDocument {
    pub id: String,
    pub title: String,
    pub content_md: String,
}

/// Request body for the email verification endpoint.  The service accepts
/// the same challenge fields as login and registration.
#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct VerifyCodeRequest {
    pub email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turnstile_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tencent_captcha_ticket: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tencent_captcha_randstr: Option<String>,
}

impl fmt::Debug for VerifyCodeRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifyCodeRequest")
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

impl VerifyCodeRequest {
    pub fn new(email: impl Into<String>) -> Self {
        Self {
            email: email.into(),
            turnstile_token: None,
            tencent_captcha_ticket: None,
            tencent_captcha_randstr: None,
        }
    }
}

/// A login that passed the password step but still requires a TOTP code.
/// The temporary token is only exposed through an accessor and is redacted
/// from debug output.
#[derive(Clone)]
pub struct TwoFactorChallenge {
    temp_token: String,
    pub email: String,
    pub masked_email: Option<String>,
}

impl TwoFactorChallenge {
    pub fn temp_token(&self) -> &str {
        &self.temp_token
    }
}

impl fmt::Debug for TwoFactorChallenge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TwoFactorChallenge")
            .field("temp_token", &"[redacted]")
            .field("email", &self.email)
            .field("masked_email", &self.masked_email)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub enum LoginOutcome {
    Authenticated(AccountSession),
    TwoFactorRequired(TwoFactorChallenge),
}

#[derive(Clone, Serialize)]
pub struct RedeemRequest {
    pub code: String,
}

impl fmt::Debug for RedeemRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RedeemRequest")
            .field("code", &"[redacted]")
            .finish()
    }
}

#[derive(Clone, Serialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

impl fmt::Debug for RefreshRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RefreshRequest")
            .field("refresh_token", &"[redacted]")
            .finish()
    }
}

#[derive(Clone, Serialize)]
pub struct LogoutRequest {
    pub refresh_token: String,
}

impl fmt::Debug for LogoutRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LogoutRequest")
            .field("refresh_token", &"[redacted]")
            .finish()
    }
}

/// Authorization-code exchange payload used by the Z8 browser login flow.
///
/// Codex++ does not own the browser callback itself, but keeping the exchange
/// in the account client preserves the same API boundary as Z8 Launch and lets
/// a future host integration pass only the one-use code across that boundary.
#[derive(Clone, Serialize)]
pub struct DesktopTokenRequest {
    pub client_id: String,
    pub code: String,
    pub code_verifier: String,
    pub redirect_uri: String,
}

impl DesktopTokenRequest {
    pub fn new(
        code: impl Into<String>,
        code_verifier: impl Into<String>,
        redirect_uri: impl Into<String>,
    ) -> Self {
        Self {
            client_id: "z8-launch".to_string(),
            code: code.into(),
            code_verifier: code_verifier.into(),
            redirect_uri: redirect_uri.into(),
        }
    }
}

impl fmt::Debug for DesktopTokenRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopTokenRequest")
            .field("client_id", &self.client_id)
            .field("code", &"[redacted]")
            .field("code_verifier", &"[redacted]")
            .field("redirect_uri", &self.redirect_uri)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountProfile {
    pub id: Option<String>,
    pub email: String,
    pub display_name: Option<String>,
}

/// Tokens are intentionally omitted from `Debug` output and only exposed by
/// accessors so callers do not accidentally put them into logs.
#[derive(Clone)]
pub struct AccountSession {
    access_token: String,
    refresh_token: Option<String>,
    pub user: AccountProfile,
    pub expires_at: Option<String>,
}

impl AccountSession {
    fn from_value(value: &Value, fallback_email: &str) -> Result<Self, AccountError> {
        let access_token =
            required_secret(value, "access_token", MAX_TOKEN_BYTES).ok_or_else(|| {
                AccountError::new(AccountErrorCode::InvalidResponse, "响应缺少 access_token")
            })?;
        let refresh_token = match value.get("refresh_token") {
            None | Some(Value::Null) => None,
            Some(_) => Some(
                required_secret(value, "refresh_token", MAX_REFRESH_TOKEN_BYTES).ok_or_else(
                    || {
                        AccountError::new(
                            AccountErrorCode::InvalidResponse,
                            "响应中的 refresh_token 无效",
                        )
                    },
                )?,
            ),
        };
        let user_value = value.get("user").and_then(Value::as_object);
        let email = user_value
            .and_then(|user| user.get("email"))
            .and_then(Value::as_str)
            .unwrap_or(fallback_email)
            .to_string();
        validate_email(&email)?;
        let profile = AccountProfile {
            id: user_value
                .and_then(|user| user.get("id"))
                .and_then(value_as_string),
            email,
            display_name: user_value
                .and_then(|user| {
                    user.get("display_name")
                        .or_else(|| user.get("displayName"))
                        .or_else(|| user.get("name"))
                })
                .and_then(Value::as_str)
                .map(str::to_string),
        };
        Ok(Self {
            access_token,
            refresh_token,
            user: profile,
            expires_at: value
                .get("expires_at")
                .or_else(|| value.get("expiresAt"))
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }

    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    pub fn refresh_token(&self) -> Option<&str> {
        self.refresh_token.as_deref()
    }

    pub fn with_tokens_from(&self, value: &Value) -> Result<Self, AccountError> {
        let mut next = Self::from_value(value, &self.user.email)?;
        // Z8 Launch treats refresh as credential rotation, not a profile
        // switch. A gateway-provided user must not silently replace the
        // authenticated identity associated with the old refresh token.
        if let Some(user) = value.get("user").and_then(Value::as_object) {
            let email_changed = user
                .get("email")
                .and_then(Value::as_str)
                .is_some_and(|email| !email.eq_ignore_ascii_case(&self.user.email));
            let id_changed = user
                .get("id")
                .and_then(value_as_string)
                .is_some_and(|id| self.user.id.as_ref().is_some_and(|old| old != &id));
            if email_changed || id_changed {
                return Err(AccountError::new(
                    AccountErrorCode::InvalidResponse,
                    "刷新响应中的账户身份不一致",
                ));
            }
        }
        next.user = self.user.clone();
        if next.expires_at.is_none() {
            next.expires_at = self.expires_at.clone();
        }
        Ok(next)
    }

    /// Convert a committed session into the payload held by the platform
    /// credential store. This value must never be written to a normal file or
    /// included in logs.
    pub fn to_persisted_value(&self) -> Value {
        json!({
            "access_token": self.access_token,
            "refresh_token": self.refresh_token,
            "user": {
                "id": self.user.id,
                "email": self.user.email,
                "display_name": self.user.display_name,
            },
            "expires_at": self.expires_at,
        })
    }

    /// Restore a session read from the platform credential store. The same
    /// token and profile validation as an API response is applied.
    pub fn from_persisted_value(value: &Value) -> Result<Self, AccountError> {
        Self::from_value(value, "")
    }
}

impl fmt::Debug for AccountSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AccountSession")
            .field("access_token", &"[redacted]")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[redacted]"),
            )
            .field("user", &self.user)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RedeemReceipt {
    pub message: Option<String>,
    #[serde(alias = "type")]
    pub kind: Option<String>,
    pub value: Option<f64>,
    pub new_balance: Option<f64>,
}

const MAX_KEY_BYTES: usize = 4096;
const MAX_REFRESH_TOKEN_BYTES: usize = 2048;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountStatus {
    pub authenticated: bool,
    pub email: Option<String>,
}

/// API key material returned by the Z8 account service. The secret is kept in
/// native memory and is never serialized to the Manager UI.
#[derive(Clone, PartialEq, Eq)]
pub struct AccountApiKey {
    pub id: String,
    pub name: String,
    pub status: String,
    secret: String,
    pub created_at: Option<String>,
    pub expires_at: Option<String>,
}

impl AccountApiKey {
    pub fn secret(&self) -> &str {
        &self.secret
    }

    /// Serialize API key material only for the platform credential store.
    pub fn to_persisted_value(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "status": self.status,
            "key": self.secret,
            "created_at": self.created_at,
            "expires_at": self.expires_at,
        })
    }

    /// Restore API key material read from the platform credential store.
    pub fn from_persisted_value(value: &Value) -> Result<Self, AccountError> {
        parse_api_keys(&json!({ "items": [value] }))?
            .into_iter()
            .next()
            .ok_or_else(|| {
                AccountError::new(AccountErrorCode::InvalidResponse, "保存的 API Key 无效")
            })
    }
}

impl fmt::Debug for AccountApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AccountApiKey")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("status", &self.status)
            .field("secret", &"[redacted]")
            .field("created_at", &self.created_at)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Clone)]
pub struct AccountClient {
    http: reqwest::Client,
    base_url: Url,
}

impl AccountClient {
    pub fn new(base_url: impl AsRef<str>) -> Result<Self, AccountError> {
        let mut url = Url::parse(base_url.as_ref().trim_end_matches('/'))
            .map_err(|_| AccountError::new(AccountErrorCode::InvalidInput, "账户 API 地址无效"))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(AccountError::new(
                AccountErrorCode::InvalidInput,
                "账户 API 地址无效",
            ));
        }
        if url.path().is_empty() {
            url.set_path("/");
        }
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(8))
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(format!("Z8-Codex/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| {
                AccountError::new(AccountErrorCode::Unavailable, "无法创建账户网络客户端")
            })?;
        Ok(Self {
            http,
            base_url: url,
        })
    }

    pub fn default() -> Result<Self, AccountError> {
        Self::new(DEFAULT_Z8_ACCOUNT_BASE_URL)
    }

    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    /// Fetch the public flags used to decide which account and captcha fields
    /// should be shown.  The response is intentionally reduced to client-safe
    /// settings before it leaves the core module.
    pub async fn auth_settings(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<AccountAuthSettings, AccountError> {
        let value = self
            .request_json(
                Method::GET,
                "settings/public",
                None,
                Value::Null,
                RequestPurpose::PublicSettings,
                cancellation,
            )
            .await?;
        parse_auth_settings(&value)
    }

    pub async fn login(
        &self,
        request: &LoginRequest,
        cancellation: &CancellationToken,
    ) -> Result<AccountSession, AccountError> {
        match self.login_outcome(request, cancellation).await? {
            LoginOutcome::Authenticated(session) => Ok(session),
            LoginOutcome::TwoFactorRequired(_) => Err(AccountError::new(
                AccountErrorCode::TwoFactorRequired,
                "账户需要完成双因素验证",
            )),
        }
    }

    /// Perform the password and captcha step.  A successful response may
    /// require a second TOTP request; that state is represented explicitly so
    /// callers never mistake a temporary token for an authenticated session.
    pub async fn login_outcome(
        &self,
        request: &LoginRequest,
        cancellation: &CancellationToken,
    ) -> Result<LoginOutcome, AccountError> {
        validate_login(request)?;
        let value = self
            .request_json(
                Method::POST,
                "auth/login",
                None,
                serde_json::to_value(request).map_err(|_| invalid_request())?,
                RequestPurpose::Login,
                cancellation,
            )
            .await?;
        parse_login_outcome(&value, &request.email)
    }

    pub async fn complete_two_factor(
        &self,
        challenge: &TwoFactorChallenge,
        totp_code: &str,
        cancellation: &CancellationToken,
    ) -> Result<AccountSession, AccountError> {
        validate_totp_code(totp_code)?;
        let value = self
            .request_json(
                Method::POST,
                "auth/login/2fa",
                None,
                json!({
                    "temp_token": challenge.temp_token(),
                    "totp_code": totp_code,
                }),
                RequestPurpose::TwoFactor,
                cancellation,
            )
            .await?;
        AccountSession::from_value(&value, &challenge.email)
    }

    /// Exchange a one-use browser authorization code for an authenticated
    /// account session. The caller owns the browser callback and only passes
    /// the short-lived code and PKCE verifier into this client.
    pub async fn exchange_desktop_token(
        &self,
        request: &DesktopTokenRequest,
        cancellation: &CancellationToken,
    ) -> Result<AccountSession, AccountError> {
        validate_desktop_token_request(request)?;
        let value = self
            .request_json(
                Method::POST,
                "auth/desktop/token",
                None,
                serde_json::to_value(request).map_err(|_| invalid_request())?,
                RequestPurpose::Login,
                cancellation,
            )
            .await?;
        let session = AccountSession::from_value(&value, "")?;
        // The Launch browser exchange is a durable sign-in boundary.  It
        // requires a refresh credential so the manager can restore the
        // account after the one-use authorization code has expired.  A
        // response containing only an access token would otherwise appear
        // authenticated in memory and then silently become unrecoverable.
        if session.refresh_token().is_none() {
            return Err(AccountError::new(
                AccountErrorCode::InvalidResponse,
                "桌面登录响应缺少 refresh_token",
            ));
        }
        Ok(session)
    }

    /// Fetch the current profile using the session access token. This mirrors
    /// Z8 Launch's restore path and deliberately returns only profile fields,
    /// leaving the caller's token ownership unchanged.
    pub async fn me(
        &self,
        session: &AccountSession,
        cancellation: &CancellationToken,
    ) -> Result<AccountProfile, AccountError> {
        let value = self
            .request_json(
                Method::GET,
                "auth/me",
                Some(session.access_token()),
                Value::Null,
                RequestPurpose::Authenticated,
                cancellation,
            )
            .await?;
        parse_profile(&value, &session.user.email)
    }

    pub async fn register(
        &self,
        request: &RegisterRequest,
        cancellation: &CancellationToken,
    ) -> Result<AccountSession, AccountError> {
        validate_register(request)?;
        let value = self
            .request_json(
                Method::POST,
                "auth/register",
                None,
                serde_json::to_value(request).map_err(|_| invalid_request())?,
                RequestPurpose::Register,
                cancellation,
            )
            .await?;
        validate_registration_embedded_key(&value)?;
        AccountSession::from_value(&value, &request.email)
    }

    pub async fn refresh(
        &self,
        session: &AccountSession,
        cancellation: &CancellationToken,
    ) -> Result<AccountSession, AccountError> {
        let refresh_token = session.refresh_token().ok_or_else(|| {
            AccountError::new(
                AccountErrorCode::SessionExpired,
                "当前账户没有可用的刷新令牌",
            )
        })?;
        let value = self
            .request_json(
                Method::POST,
                "auth/refresh",
                None,
                json!(RefreshRequest {
                    refresh_token: refresh_token.to_string()
                }),
                RequestPurpose::Authenticated,
                cancellation,
            )
            .await?;
        let next = session.with_tokens_from(&value)?;
        if next.refresh_token().is_none() {
            // Z8 rotates the refresh credential on every successful refresh;
            // accepting an access-only response would make expiry recovery
            // impossible and would silently lose the saved account.
            return Err(AccountError::new(
                AccountErrorCode::InvalidResponse,
                "刷新响应缺少 refresh_token",
            ));
        }
        Ok(next)
    }

    pub async fn send_verification_code(
        &self,
        request: &VerifyCodeRequest,
        cancellation: &CancellationToken,
    ) -> Result<(), AccountError> {
        validate_verify_code_request(request)?;
        self.request_json(
            Method::POST,
            "auth/send-verify-code",
            None,
            serde_json::to_value(request).map_err(|_| invalid_request())?,
            RequestPurpose::VerifyCode,
            cancellation,
        )
        .await
        .map(|_| ())
    }

    pub async fn redeem(
        &self,
        session: &AccountSession,
        code: &str,
        cancellation: &CancellationToken,
    ) -> Result<RedeemReceipt, AccountError> {
        validate_redeem_code(code)?;
        let value = self
            .request_json(
                Method::POST,
                "redeem",
                Some(session.access_token()),
                json!(RedeemRequest {
                    code: code.to_string()
                }),
                RequestPurpose::Authenticated,
                cancellation,
            )
            .await?;
        parse_redeem_receipt(&value)
    }

    pub async fn logout(
        &self,
        session: &AccountSession,
        cancellation: &CancellationToken,
    ) -> Result<(), AccountError> {
        let Some(refresh_token) = session.refresh_token() else {
            return Ok(());
        };
        self.request_json(
            Method::POST,
            "auth/logout",
            Some(session.access_token()),
            json!(LogoutRequest {
                refresh_token: refresh_token.to_string()
            }),
            RequestPurpose::Authenticated,
            cancellation,
        )
        .await
        .map(|_| ())
    }

    pub async fn list_api_keys(
        &self,
        session: &AccountSession,
        cancellation: &CancellationToken,
    ) -> Result<Vec<AccountApiKey>, AccountError> {
        let value = self
            .request_json(
                Method::GET,
                "keys?page=1&page_size=1000",
                Some(session.access_token()),
                Value::Null,
                RequestPurpose::Authenticated,
                cancellation,
            )
            .await?;
        parse_api_keys(&value)
    }

    /// Compatibility spelling used by the Launch account module.
    pub async fn list_keys(
        &self,
        session: &AccountSession,
        cancellation: &CancellationToken,
    ) -> Result<Vec<AccountApiKey>, AccountError> {
        self.list_api_keys(session, cancellation).await
    }

    async fn request_json(
        &self,
        method: Method,
        route: &str,
        bearer: Option<&str>,
        payload: Value,
        purpose: RequestPurpose,
        cancellation: &CancellationToken,
    ) -> Result<Value, AccountError> {
        if cancellation.is_cancelled() {
            return Err(cancelled());
        }
        let url = self.endpoint(route)?;
        let mut request = self
            .http
            .request(method, url)
            .header("accept", "application/json");
        if let Some(token) = bearer {
            request = request.bearer_auth(token);
        }
        let request = if payload.is_null() {
            request
        } else {
            request.json(&payload)
        };
        let response = tokio::select! {
            response = request.send() => response.map_err(map_transport_error),
            _ = cancellation.cancelled() => Err(cancelled()),
        }?;
        let value = parse_response(response, purpose, cancellation).await?;
        if cancellation.is_cancelled() {
            return Err(cancelled());
        }
        Ok(value)
    }

    fn endpoint(&self, route: &str) -> Result<Url, AccountError> {
        if route.is_empty()
            || route.contains("..")
            || route.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(AccountError::new(
                AccountErrorCode::InvalidInput,
                "账户 API 路径无效",
            ));
        }
        let (path, query) = route.split_once('?').unwrap_or((route, ""));
        let version = self.base_url.path().trim_matches('/');
        if version.is_empty() || version.contains('/') || version.contains('.') {
            return Err(AccountError::new(
                AccountErrorCode::InvalidInput,
                "账户 API 地址无效",
            ));
        }
        let normalized_route = path.trim_matches('/');
        if normalized_route.is_empty()
            || normalized_route.contains("..")
            || normalized_route.bytes().any(|byte| byte.is_ascii_control())
            || query.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(AccountError::new(
                AccountErrorCode::InvalidInput,
                "账户 API 路径无效",
            ));
        }
        let mut url = self.base_url.clone();
        url.set_path(&format!("/api/{version}/{normalized_route}"));
        url.set_query((!query.is_empty()).then_some(query));
        url.set_fragment(None);
        Ok(url)
    }
}

fn parse_api_keys(value: &Value) -> Result<Vec<AccountApiKey>, AccountError> {
    let items = value
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            AccountError::new(
                AccountErrorCode::InvalidResponse,
                "账户响应缺少 API Key 列表",
            )
        })?;
    let mut keys = Vec::with_capacity(items.len().min(1000));
    for item in items.iter().take(1000) {
        let object = item.as_object().ok_or_else(|| {
            AccountError::new(AccountErrorCode::InvalidResponse, "账户 API Key 响应无效")
        })?;
        let id = object.get("id").and_then(value_as_string).ok_or_else(|| {
            AccountError::new(AccountErrorCode::InvalidResponse, "账户 API Key 缺少 id")
        })?;
        if id.parse::<i64>().ok().filter(|id| *id > 0).is_none() {
            return Err(AccountError::new(
                AccountErrorCode::InvalidResponse,
                "账户 API Key id 无效",
            ));
        }
        let secret = object
            .get("key")
            .and_then(Value::as_str)
            .filter(|value| api_key_is_valid(value))
            .map(str::to_string)
            .ok_or_else(|| {
                AccountError::new(AccountErrorCode::InvalidResponse, "账户 API Key 无效")
            })?;
        let name = object
            .get("name")
            .and_then(Value::as_str)
            .filter(|value| {
                !value.trim().is_empty()
                    && value.len() <= 128
                    && !value.chars().any(char::is_control)
            })
            .unwrap_or("未命名密钥")
            .to_string();
        let status = object
            .get("status")
            .and_then(Value::as_str)
            .filter(|value| {
                !value.trim().is_empty()
                    && value.len() <= 32
                    && !value.chars().any(char::is_control)
            })
            .unwrap_or("unknown")
            .to_string();
        keys.push(AccountApiKey {
            id,
            name,
            status,
            secret,
            created_at: object
                .get("created_at")
                .or_else(|| object.get("createdAt"))
                .and_then(Value::as_str)
                .map(str::to_string),
            expires_at: object
                .get("expires_at")
                .or_else(|| object.get("expiresAt"))
                .and_then(Value::as_str)
                .map(str::to_string),
        });
    }
    Ok(keys)
}

fn validate_registration_embedded_key(value: &Value) -> Result<(), AccountError> {
    let Some(key_value) = value.get("api_key") else {
        // Older registration responses contain only tokens and user; the
        // account's keys are then fetched from /keys.
        return Ok(());
    };
    let valid = (|| {
        let user_id = value
            .get("user")?
            .get("id")?
            .as_i64()
            .filter(|id| *id > 0)?;
        let key = key_value.as_object()?;
        key.get("id")?.as_i64().filter(|id| *id > 0)?;
        key.get("group_id")?.as_i64().filter(|id| *id > 0)?;
        let key_user_id = key.get("user_id")?.as_i64()?;
        let secret = key.get("key")?.as_str()?;
        (key_user_id == user_id && api_key_is_valid(secret)).then_some(())
    })()
    .is_some();
    if !valid {
        return Err(AccountError::new(
            AccountErrorCode::InvalidResponse,
            "注册响应中的 API Key 归属无效",
        ));
    }
    Ok(())
}

fn parse_auth_settings(value: &Value) -> Result<AccountAuthSettings, AccountError> {
    let object = value.as_object().ok_or_else(|| {
        AccountError::new(AccountErrorCode::InvalidResponse, "账户设置响应结构无效")
    })?;
    let login_agreement_enabled = match object.get("login_agreement_enabled") {
        None => false,
        Some(Value::Bool(enabled)) => *enabled,
        Some(_) => {
            return Err(AccountError::new(
                AccountErrorCode::InvalidResponse,
                "登录协议启用状态无效",
            ));
        }
    };
    let login_agreement_mode = bounded_public_text(object.get("login_agreement_mode"), 32);
    let login_agreement_documents = parse_login_agreement_documents(
        object.get("login_agreement_documents"),
        login_agreement_enabled,
    )?;
    if login_agreement_enabled
        && !matches!(login_agreement_mode.as_deref(), Some("checkbox" | "modal"))
    {
        return Err(AccountError::new(
            AccountErrorCode::InvalidResponse,
            "登录协议模式无效",
        ));
    }
    Ok(AccountAuthSettings {
        registration_enabled: object
            .get("registration_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        email_verify_enabled: object
            .get("email_verify_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        invitation_code_enabled: object
            .get("invitation_code_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        promo_code_enabled: object
            .get("promo_code_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        turnstile_enabled: object
            .get("turnstile_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        turnstile_site_key: bounded_public_text(object.get("turnstile_site_key"), 256),
        tencent_captcha_enabled: object
            .get("tencent_captcha_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        tencent_captcha_app_id: bounded_public_text(object.get("tencent_captcha_app_id"), 128),
        tencent_captcha_region: bounded_public_text(object.get("tencent_captcha_region"), 32),
        aliyun_captcha_enabled: object
            .get("aliyun_captcha_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        aliyun_captcha_scene_id: bounded_public_text(object.get("aliyun_captcha_scene_id"), 128),
        aliyun_captcha_prefix: bounded_public_text(object.get("aliyun_captcha_prefix"), 128),
        aliyun_captcha_region: bounded_public_text(object.get("aliyun_captcha_region"), 32),
        login_agreement_enabled,
        login_agreement_mode,
        login_agreement_revision: bounded_public_text(object.get("login_agreement_revision"), 128),
        login_agreement_updated_at: bounded_public_text(
            object.get("login_agreement_updated_at"),
            128,
        ),
        login_agreement_documents,
    })
}

fn parse_login_agreement_documents(
    value: Option<&Value>,
    required: bool,
) -> Result<Vec<LoginAgreementDocument>, AccountError> {
    let Some(value) = value else {
        if required {
            return Err(AccountError::new(
                AccountErrorCode::InvalidResponse,
                "登录协议文档缺失",
            ));
        }
        return Ok(Vec::new());
    };
    let documents = value.as_array().ok_or_else(|| {
        AccountError::new(AccountErrorCode::InvalidResponse, "登录协议文档结构无效")
    })?;
    if documents.len() > 8 || (required && documents.is_empty()) {
        return Err(AccountError::new(
            AccountErrorCode::InvalidResponse,
            "登录协议文档数量无效",
        ));
    }
    let mut parsed = Vec::with_capacity(documents.len());
    let mut total_content_bytes = 0usize;
    for document in documents {
        let object = document.as_object().ok_or_else(|| {
            AccountError::new(AccountErrorCode::InvalidResponse, "登录协议文档结构无效")
        })?;
        let id = bounded_public_text(object.get("id"), 128).filter(|text| !text.trim().is_empty());
        let title =
            bounded_public_text(object.get("title"), 256).filter(|text| !text.trim().is_empty());
        let content = object
            .get("content_md")
            .and_then(Value::as_str)
            .filter(|text| {
                !text.trim().is_empty()
                    && text.len() <= 32 * 1024
                    && !text.chars().any(|character| {
                        character.is_control() && !matches!(character, '\n' | '\r' | '\t')
                    })
            });
        let (Some(id), Some(title), Some(content)) = (id, title, content) else {
            return Err(AccountError::new(
                AccountErrorCode::InvalidResponse,
                "登录协议文档内容无效",
            ));
        };
        total_content_bytes += content.len();
        if total_content_bytes > 128 * 1024
            || parsed
                .iter()
                .any(|existing: &LoginAgreementDocument| existing.id == id)
        {
            return Err(AccountError::new(
                AccountErrorCode::InvalidResponse,
                "登录协议文档内容无效",
            ));
        }
        parsed.push(LoginAgreementDocument {
            id,
            title,
            content_md: content.to_string(),
        });
    }
    Ok(parsed)
}

fn parse_login_outcome(value: &Value, fallback_email: &str) -> Result<LoginOutcome, AccountError> {
    let requires_2fa = value
        .get("requires_2fa")
        .or_else(|| value.get("require_2fa"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !requires_2fa {
        return Ok(LoginOutcome::Authenticated(AccountSession::from_value(
            value,
            fallback_email,
        )?));
    }
    let temp_token = required_secret(value, "temp_token", MAX_TOKEN_BYTES).ok_or_else(|| {
        AccountError::new(
            AccountErrorCode::InvalidResponse,
            "双因素验证响应缺少临时令牌",
        )
    })?;
    let email = value
        .get("user_email")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("user")
                .and_then(Value::as_object)
                .and_then(|user| user.get("email"))
                .and_then(Value::as_str)
        })
        .unwrap_or(fallback_email)
        .to_string();
    validate_email(&email)?;
    let masked_email = value
        .get("user_email_masked")
        .or_else(|| value.get("masked_email"))
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty() && text.len() <= MAX_EMAIL_BYTES)
        .map(str::to_string);
    Ok(LoginOutcome::TwoFactorRequired(TwoFactorChallenge {
        temp_token,
        email,
        masked_email,
    }))
}

fn parse_profile(value: &Value, fallback_email: &str) -> Result<AccountProfile, AccountError> {
    let object = value.as_object().ok_or_else(|| {
        AccountError::new(AccountErrorCode::InvalidResponse, "账户资料响应结构无效")
    })?;
    let user = object
        .get("user")
        .and_then(Value::as_object)
        .unwrap_or(object);
    let email = user
        .get("email")
        .and_then(Value::as_str)
        .unwrap_or(fallback_email)
        .to_string();
    validate_email(&email)?;
    Ok(AccountProfile {
        id: user.get("id").and_then(value_as_string),
        email,
        display_name: user
            .get("display_name")
            .or_else(|| user.get("displayName"))
            .or_else(|| user.get("name"))
            .and_then(Value::as_str)
            .filter(|value| {
                !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
            })
            .map(str::to_string),
    })
}

fn bounded_public_text(value: Option<&Value>, max_bytes: usize) -> Option<String> {
    let text = value?.as_str()?;
    if text.is_empty() || text.len() > max_bytes || text.chars().any(char::is_control) {
        return None;
    }
    Some(text.to_string())
}

/// Session state owned by the Codex++ manager.  The generation gate protects
/// the old session while requests are in flight and prevents a late response
/// from a cancelled operation from becoming the active account.
#[derive(Clone, Default)]
pub struct AccountSessionStore {
    state: Arc<Mutex<SessionState>>,
}

#[derive(Default)]
struct SessionState {
    current: Option<AccountSession>,
    generation: u64,
}

impl AccountSessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> Option<AccountSession> {
        self.state
            .lock()
            .ok()
            .and_then(|state| state.current.clone())
    }

    /// Restore a session that was read from the platform credential store.
    /// Restoring advances the generation so any request created before app
    /// startup cannot overwrite the recovered account.
    pub fn restore(&self, session: AccountSession) -> Result<AccountSession, AccountError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AccountError::new(AccountErrorCode::Unavailable, "账户会话状态不可用"))?;
        state.generation = state.generation.wrapping_add(1);
        state.current = Some(session.clone());
        Ok(session)
    }

    pub fn status(&self) -> AccountStatus {
        match self.snapshot() {
            Some(session) => AccountStatus {
                authenticated: true,
                email: Some(session.user.email),
            },
            None => AccountStatus {
                authenticated: false,
                email: None,
            },
        }
    }

    /// Start an operation. Starting a newer operation supersedes older
    /// operations, even when the older HTTP request eventually succeeds.
    pub fn begin(&self) -> AccountOperation {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.generation = state.generation.wrapping_add(1);
        let generation = state.generation;
        AccountOperation {
            store: self.clone(),
            generation,
            cancellation: CancellationToken::new(),
        }
    }

    fn commit(
        &self,
        generation: u64,
        cancellation: &CancellationToken,
        session: AccountSession,
    ) -> Result<AccountSession, AccountError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AccountError::new(AccountErrorCode::Unavailable, "账户会话状态不可用"))?;
        if cancellation.is_cancelled() || state.generation != generation {
            return Err(cancelled());
        }
        state.current = Some(session.clone());
        Ok(session)
    }

    fn clear(&self, generation: u64, cancellation: &CancellationToken) -> Result<(), AccountError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AccountError::new(AccountErrorCode::Unavailable, "账户会话状态不可用"))?;
        if cancellation.is_cancelled() || state.generation != generation {
            return Err(cancelled());
        }
        state.current = None;
        Ok(())
    }

    fn invalidate(&self, generation: u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.generation == generation {
            state.generation = state.generation.wrapping_add(1);
        }
    }
}

pub struct AccountOperation {
    store: AccountSessionStore,
    generation: u64,
    cancellation: CancellationToken,
}

impl AccountOperation {
    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    pub fn cancel(&self) {
        self.cancellation.cancel();
        self.store.invalidate(self.generation);
    }

    pub fn commit(&self, session: AccountSession) -> Result<AccountSession, AccountError> {
        if self.cancellation.is_cancelled() {
            return Err(cancelled());
        }
        self.store
            .commit(self.generation, &self.cancellation, session)
    }

    pub fn logout(&self) -> Result<(), AccountError> {
        if self.cancellation.is_cancelled() {
            return Err(cancelled());
        }
        self.store.clear(self.generation, &self.cancellation)
    }
}

fn parse_response<'a>(
    response: reqwest::Response,
    purpose: RequestPurpose,
    cancellation: &'a CancellationToken,
) -> impl std::future::Future<Output = Result<Value, AccountError>> + 'a {
    async move {
        let status = response.status();
        if response
            .headers()
            .get("cf-mitigated")
            .and_then(|value| value.to_str().ok())
            == Some("challenge")
        {
            return Err(AccountError::new(
                AccountErrorCode::SecurityChallenge,
                "账户服务要求完成安全校验",
            ));
        }
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        let bytes = tokio::select! {
            bytes = response.bytes() => bytes.map_err(map_transport_error)?,
            _ = cancellation.cancelled() => return Err(cancelled()),
        };
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(AccountError::new(
                AccountErrorCode::InvalidResponse,
                "账户响应过大",
            ));
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
            if status.is_success() {
                AccountError::new(AccountErrorCode::InvalidResponse, "账户响应不是有效 JSON")
            } else {
                map_http_status(status, purpose)
            }
        })?;
        decode_envelope(value, status, purpose).map_err(|error| error.with_retry_after(retry_after))
    }
}

fn decode_envelope(
    value: Value,
    status: StatusCode,
    purpose: RequestPurpose,
) -> Result<Value, AccountError> {
    let object = value
        .as_object()
        .ok_or_else(|| AccountError::new(AccountErrorCode::InvalidResponse, "账户响应结构无效"))?;
    if !status.is_success() {
        return Err(map_api_error(object, map_http_status(status, purpose)));
    }
    let code = object.get("code").and_then(Value::as_i64).unwrap_or(0);
    if code != 0 {
        return Err(map_api_error(object, map_http_status_code(code, purpose)));
    }
    object
        .get("data")
        .cloned()
        .ok_or_else(|| AccountError::new(AccountErrorCode::InvalidResponse, "账户响应缺少 data"))
}

fn map_api_error(object: &Map<String, Value>, fallback: AccountError) -> AccountError {
    let reason = api_error_reason(object).unwrap_or_default();
    let mapped = match reason {
        "TOKEN_REVOKED"
        | "USER_NOT_ACTIVE"
        | "SESSION_BINDING_MISMATCH"
        | "REFRESH_TOKEN_INVALID"
        | "REFRESH_TOKEN_EXPIRED"
        | "REFRESH_TOKEN_REUSED" => AccountErrorCode::SessionRevoked,
        "TURNSTILE_VERIFICATION_FAILED"
        | "TENCENT_CAPTCHA_VERIFICATION_FAILED"
        | "ALIYUN_CAPTCHA_VERIFICATION_FAILED" => AccountErrorCode::CaptchaFailed,
        "TURNSTILE_NOT_CONFIGURED"
        | "TENCENT_CAPTCHA_NOT_CONFIGURED"
        | "ALIYUN_CAPTCHA_NOT_CONFIGURED"
        | "CAPTCHA_PROVIDER_CONFLICT" => AccountErrorCode::CaptchaUnavailable,
        "EMAIL_EXISTS" => AccountErrorCode::EmailExists,
        "EMAIL_RESERVED" => AccountErrorCode::EmailReserved,
        "EMAIL_VERIFY_REQUIRED" => AccountErrorCode::EmailVerifyRequired,
        "EMAIL_SUFFIX_NOT_ALLOWED" => AccountErrorCode::EmailSuffixNotAllowed,
        "EMAIL_DOMAIN_REGISTRATION_LIMIT" => AccountErrorCode::EmailDomainLimit,
        "INVITATION_CODE_REQUIRED" => AccountErrorCode::InvitationRequired,
        "INVITATION_CODE_INVALID" => AccountErrorCode::InvitationInvalid,
        "REGISTRATION_DISABLED" => AccountErrorCode::RegistrationDisabled,
        "BACKEND_MODE_ADMIN_ONLY" => AccountErrorCode::BackendAdminOnly,
        "REGISTRATION_DEFAULT_GROUP_UNAVAILABLE" => {
            AccountErrorCode::RegistrationDefaultGroupUnavailable
        }
        "SERVICE_UNAVAILABLE" => AccountErrorCode::Unavailable,
        "INVALID_EMAIL" => AccountErrorCode::InvalidInput,
        "REGISTRATION_API_KEY_PROVISION_FAILED" => {
            let completed = object
                .get("metadata")
                .and_then(Value::as_object)
                .and_then(|metadata| metadata.get("registration_completed"))
                .and_then(|value| match value {
                    Value::Bool(value) => Some(*value),
                    Value::String(value) => value.parse::<bool>().ok(),
                    _ => None,
                })
                .unwrap_or(false);
            if completed {
                AccountErrorCode::RegistrationApiKeyProvisionFailed
            } else {
                AccountErrorCode::RegistrationUnavailable
            }
        }
        _ => return fallback,
    };
    AccountError::new(mapped, public_message(object, mapped.as_str()))
}

/// Z8 responses have historically put the machine-readable reason at the
/// top level. Some gateways wrap it in an `error` object, so accept both
/// shapes while keeping the mapping deterministic.
fn api_error_reason(object: &Map<String, Value>) -> Option<&str> {
    object
        .get("reason")
        .and_then(Value::as_str)
        .or_else(|| object.get("error").and_then(Value::as_str))
        .or_else(|| {
            object
                .get("error")
                .and_then(Value::as_object)
                .and_then(|error| {
                    error
                        .get("reason")
                        .or_else(|| error.get("code"))
                        .or_else(|| error.get("type"))
                        .and_then(Value::as_str)
                })
        })
        .or_else(|| object.get("code").and_then(Value::as_str))
}

fn public_message(object: &Map<String, Value>, fallback: &str) -> String {
    object
        .get("message")
        .or_else(|| object.get("msg"))
        .and_then(Value::as_str)
        .filter(|value| !value.chars().any(char::is_control))
        .map(str::to_string)
        .unwrap_or_else(|| fallback.to_string())
}

fn map_http_status(status: StatusCode, purpose: RequestPurpose) -> AccountError {
    let code = match status {
        StatusCode::UNAUTHORIZED => match purpose {
            RequestPurpose::Login => AccountErrorCode::InvalidCredentials,
            RequestPurpose::TwoFactor => AccountErrorCode::TwoFactorInvalid,
            _ => AccountErrorCode::SessionExpired,
        },
        StatusCode::FORBIDDEN => match purpose {
            RequestPurpose::Register => AccountErrorCode::RegistrationDisabled,
            _ => AccountErrorCode::Forbidden,
        },
        StatusCode::REQUEST_TIMEOUT => AccountErrorCode::Timeout,
        StatusCode::CONFLICT => AccountErrorCode::Conflict,
        StatusCode::TOO_MANY_REQUESTS => AccountErrorCode::RateLimited,
        status if status.is_client_error() => match purpose {
            RequestPurpose::Register => AccountErrorCode::RegistrationInvalid,
            RequestPurpose::VerifyCode => AccountErrorCode::VerificationFailed,
            _ => AccountErrorCode::InvalidInput,
        },
        status if status.is_server_error() => match purpose {
            RequestPurpose::Register => AccountErrorCode::RegistrationUnavailable,
            _ => AccountErrorCode::Unavailable,
        },
        _ => AccountErrorCode::RequestFailed,
    };
    AccountError::new(code, code.as_str())
}

fn map_http_status_code(status: i64, purpose: RequestPurpose) -> AccountError {
    u16::try_from(status)
        .ok()
        .and_then(|status| StatusCode::from_u16(status).ok())
        .map(|status| map_http_status(status, purpose))
        .unwrap_or_else(|| AccountError::new(AccountErrorCode::RequestFailed, "账户请求失败"))
}

fn map_transport_error(error: reqwest::Error) -> AccountError {
    if error.is_timeout() {
        AccountError::new(AccountErrorCode::Timeout, "账户请求超时")
    } else {
        AccountError::new(AccountErrorCode::Unavailable, "账户服务暂时不可用")
    }
}

fn validate_login(request: &LoginRequest) -> Result<(), AccountError> {
    validate_email(&request.email)?;
    validate_password(&request.password)?;
    validate_captcha_fields(
        request.turnstile_token.as_deref(),
        request.tencent_captcha_ticket.as_deref(),
        request.tencent_captcha_randstr.as_deref(),
    )
}

fn validate_register(request: &RegisterRequest) -> Result<(), AccountError> {
    validate_email(&request.email)?;
    validate_password(&request.password)?;
    validate_optional_code(request.verify_code.as_deref())?;
    validate_captcha_fields(
        request.turnstile_token.as_deref(),
        request.tencent_captcha_ticket.as_deref(),
        request.tencent_captcha_randstr.as_deref(),
    )?;
    for value in [
        request.promo_code.as_deref(),
        request.invitation_code.as_deref(),
        request.aff_code.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        validate_code_field(value)?;
    }
    Ok(())
}

fn validate_verify_code_request(request: &VerifyCodeRequest) -> Result<(), AccountError> {
    validate_email(&request.email)?;
    validate_captcha_fields(
        request.turnstile_token.as_deref(),
        request.tencent_captcha_ticket.as_deref(),
        request.tencent_captcha_randstr.as_deref(),
    )
}

fn validate_desktop_token_request(request: &DesktopTokenRequest) -> Result<(), AccountError> {
    for value in [
        request.client_id.as_str(),
        request.code.as_str(),
        request.code_verifier.as_str(),
        request.redirect_uri.as_str(),
    ] {
        if value.is_empty()
            || value.len() > 4096
            || !value.is_ascii()
            || value
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
        {
            return Err(invalid_request());
        }
    }
    Ok(())
}

fn validate_captcha_fields(
    turnstile: Option<&str>,
    tencent_ticket: Option<&str>,
    tencent_randstr: Option<&str>,
) -> Result<(), AccountError> {
    for value in [turnstile, tencent_ticket, tencent_randstr]
        .into_iter()
        .flatten()
    {
        if value.is_empty() {
            continue;
        }
        if value.len() > MAX_TOKEN_BYTES || !value.is_ascii() {
            return Err(invalid_request());
        }
        if value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
        {
            return Err(invalid_request());
        }
    }
    Ok(())
}

fn validate_optional_code(value: Option<&str>) -> Result<(), AccountError> {
    if let Some(value) = value {
        validate_code_field(value)?;
    }
    Ok(())
}

fn validate_code_field(value: &str) -> Result<(), AccountError> {
    if value.len() > MAX_CODE_BYTES
        || value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(invalid_request());
    }
    Ok(())
}

fn validate_email(email: &str) -> Result<(), AccountError> {
    if email.is_empty()
        || email.len() > MAX_EMAIL_BYTES
        || email
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
        || !email.contains('@')
    {
        return Err(AccountError::new(
            AccountErrorCode::InvalidInput,
            "邮箱地址无效",
        ));
    }
    Ok(())
}

fn validate_password(password: &str) -> Result<(), AccountError> {
    if password.chars().count() < 6
        || password.len() > MAX_TOKEN_BYTES
        || password.chars().any(char::is_control)
    {
        return Err(invalid_request());
    }
    Ok(())
}

fn validate_totp_code(code: &str) -> Result<(), AccountError> {
    if code.len() < 4
        || code.len() > 32
        || !code.chars().all(|character| character.is_ascii_digit())
    {
        return Err(AccountError::new(
            AccountErrorCode::InvalidInput,
            "双因素验证码无效",
        ));
    }
    Ok(())
}

fn validate_redeem_code(code: &str) -> Result<(), AccountError> {
    if code.is_empty()
        || code.len() > MAX_CODE_BYTES
        || code
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(AccountError::new(
            AccountErrorCode::InvalidInput,
            "兑换码无效",
        ));
    }
    Ok(())
}

fn parse_redeem_receipt(value: &Value) -> Result<RedeemReceipt, AccountError> {
    let object = value
        .as_object()
        .ok_or_else(|| AccountError::new(AccountErrorCode::InvalidResponse, "兑换响应结构无效"))?;
    let number = |key: &str| object.get(key).and_then(Value::as_f64);
    let value = number("value");
    let new_balance = number("new_balance");
    if value.is_some_and(|amount| !amount.is_finite() || amount < 0.0)
        || new_balance.is_some_and(|amount| !amount.is_finite() || amount < 0.0)
    {
        return Err(AccountError::new(
            AccountErrorCode::InvalidResponse,
            "兑换响应中的金额无效",
        ));
    }
    Ok(RedeemReceipt {
        message: object
            .get("message")
            .and_then(Value::as_str)
            .filter(|text| text.len() <= 256 && !text.chars().any(char::is_control))
            .map(str::to_string),
        kind: object
            .get("kind")
            .or_else(|| object.get("type"))
            .and_then(Value::as_str)
            .filter(|text| text.len() <= 64 && !text.chars().any(char::is_control))
            .map(str::to_string),
        value,
        new_balance,
    })
}

fn required_secret(value: &Value, key: &str, max_bytes: usize) -> Option<String> {
    let text = value.get(key)?.as_str()?;
    if text.is_empty()
        || text.len() > max_bytes
        || !text.is_ascii()
        || text
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        None
    } else {
        Some(text.to_string())
    }
}

fn api_key_is_valid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_KEY_BYTES
        && value.is_ascii()
        && !value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
}

fn value_as_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_i64().map(|id| id.to_string()))
}

fn invalid_request() -> AccountError {
    AccountError::new(AccountErrorCode::InvalidInput, "账户请求参数无效")
}

fn cancelled() -> AccountError {
    AccountError::new(AccountErrorCode::Cancelled, "账户请求已取消")
}

#[derive(Clone, Copy)]
enum RequestPurpose {
    Login,
    Register,
    VerifyCode,
    TwoFactor,
    PublicSettings,
    Authenticated,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{sleep, Duration};
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn session_value(access: &str, refresh: &str, email: &str) -> Value {
        json!({
            "access_token": access,
            "refresh_token": refresh,
            "user": {"id": 7, "email": email, "name": "Z8 User"}
        })
    }

    #[test]
    fn public_agreement_settings_accept_realistic_documents_and_serialize_for_ui() {
        let settings = parse_auth_settings(&json!({
            "login_agreement_enabled": true,
            "login_agreement_mode": "checkbox",
            "login_agreement_revision": "revision-1",
            "login_agreement_updated_at": "2026-09-26T00:00:00Z",
            "login_agreement_documents": [
                {"id": "terms", "title": "服务条款", "content_md": "a".repeat(8642)},
                {"id": "regions", "title": "支持的国家和地区", "content_md": "b".repeat(5280)},
                {"id": "privacy", "title": "隐私政策", "content_md": "c".repeat(2904)},
                {"id": "usage", "title": "使用政策", "content_md": "d".repeat(5883)}
            ]
        }))
        .unwrap();
        assert_eq!(settings.login_agreement_documents.len(), 4);
        assert_eq!(settings.login_agreement_documents[0].content_md.len(), 8642);
        let serialized = serde_json::to_value(settings).unwrap();
        assert_eq!(serialized["loginAgreementEnabled"], true);
        assert_eq!(serialized["loginAgreementMode"], "checkbox");
        assert_eq!(
            serialized["loginAgreementDocuments"][0]["contentMd"]
                .as_str()
                .unwrap()
                .len(),
            8642
        );
    }

    #[test]
    fn public_agreement_settings_fail_closed_when_required_content_is_invalid() {
        let cases = [
            json!({"login_agreement_enabled": "false"}),
            json!({"login_agreement_enabled": true, "login_agreement_mode": "checkbox"}),
            json!({"login_agreement_enabled": true, "login_agreement_mode": "checkbox", "login_agreement_documents": []}),
            json!({"login_agreement_enabled": true, "login_agreement_mode": "unknown", "login_agreement_documents": [{"id":"terms","title":"Terms","content_md":"text"}]}),
            json!({"login_agreement_enabled": true, "login_agreement_mode": "checkbox", "login_agreement_documents": [{"id":"terms","title":"Terms"}]}),
            json!({"login_agreement_enabled": true, "login_agreement_mode": "checkbox", "login_agreement_documents": [{"id":"terms","title":"Terms","content_md":"x".repeat(32 * 1024 + 1)}]}),
            json!({"login_agreement_enabled": true, "login_agreement_mode": "checkbox", "login_agreement_documents": [{"id":"terms","title":"Terms","content_md":"\u{0000}"}]}),
        ];
        for value in cases {
            let error = parse_auth_settings(&value).unwrap_err();
            assert_eq!(error.code, AccountErrorCode::InvalidResponse);
        }
    }

    #[test]
    fn public_agreement_settings_allow_legacy_disabled_response() {
        let settings = parse_auth_settings(&json!({"registration_enabled": true})).unwrap();
        assert!(!settings.login_agreement_enabled);
        assert!(settings.login_agreement_documents.is_empty());
        assert_eq!(settings.login_agreement_mode, None);
    }

    #[tokio::test]
    async fn login_parses_enveloped_response_without_logging_tokens() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/login"))
            .and(body_json(
                json!({"email":"user@example.com","password":"secret123"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0,
                "data": session_value("access-secret", "refresh-secret", "user@example.com")
            })))
            .mount(&server)
            .await;
        let client = AccountClient::new(format!("{}/v1", server.uri())).unwrap();
        let session = client
            .login(
                &LoginRequest::new("user@example.com", "secret123"),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(session.access_token(), "access-secret");
        assert!(!format!("{session:?}").contains("access-secret"));
        assert!(!format!("{session:?}").contains("refresh-secret"));
    }

    #[tokio::test]
    async fn list_api_keys_keeps_secret_out_of_debug_output() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/keys"))
            .and(header("authorization", "Bearer access-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0,
                "data": {"items": [{
                    "id": 42,
                    "name": "主 Key",
                    "key": "z8-secret-key",
                    "status": "active"
                }]}
            })))
            .mount(&server)
            .await;
        let client = AccountClient::new(format!("{}/v1", server.uri())).unwrap();
        let session = AccountSession::from_value(
            &session_value("access-secret", "refresh-secret", "user@example.com"),
            "user@example.com",
        )
        .unwrap();
        let keys = client
            .list_api_keys(&session, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(keys[0].id, "42");
        assert_eq!(keys[0].secret(), "z8-secret-key");
        assert!(!format!("{:?}", keys[0]).contains("z8-secret-key"));
    }

    #[tokio::test]
    async fn register_posts_optional_fields_and_parses_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/register"))
            .and(body_json(json!({
                "email":"new@example.com",
                "password":"secret123",
                "invitation_code":"WELCOME"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0,
                "data": session_value("access-new", "refresh-new", "new@example.com")
            })))
            .mount(&server)
            .await;
        let mut request = RegisterRequest::new("new@example.com", "secret123");
        request.invitation_code = Some("WELCOME".into());
        let client = AccountClient::new(format!("{}/v1", server.uri())).unwrap();
        let session = client
            .register(&request, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(session.user.email, "new@example.com");
    }

    #[tokio::test]
    async fn refresh_redeem_and_logout_use_bearer_and_stable_models() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .and(body_json(json!({"refresh_token":"refresh-old"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0,
                "data": session_value("access-next", "refresh-next", "user@example.com")
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/redeem"))
            .and(header("authorization", "Bearer access-old"))
            .and(body_json(json!({"code":"Z8-123"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0,
                "data": {"message":"兑换成功","kind":"days","value":30.0,"new_balance":30.0}
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/logout"))
            .and(header("authorization", "Bearer access-old"))
            .and(body_json(json!({"refresh_token":"refresh-old"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":{}})))
            .mount(&server)
            .await;
        let client = AccountClient::new(format!("{}/v1", server.uri())).unwrap();
        let old = AccountSession::from_value(
            &session_value("access-old", "refresh-old", "user@example.com"),
            "user@example.com",
        )
        .unwrap();
        let next = client
            .refresh(&old, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(next.access_token(), "access-next");
        let receipt = client
            .redeem(&old, "Z8-123", &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(receipt.value, Some(30.0));
        client
            .logout(&old, &CancellationToken::new())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn refresh_token_only_response_preserves_restored_profile() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .and(body_json(json!({"refresh_token":"refresh-old"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0,
                "data": {
                    "access_token": "access-next",
                    "refresh_token": "refresh-next"
                }
            })))
            .mount(&server)
            .await;

        let client = AccountClient::new(format!("{}/v1", server.uri())).unwrap();
        let old = AccountSession::from_value(
            &json!({
                "access_token": "access-old",
                "refresh_token": "refresh-old",
                "expires_at": "2026-10-01T00:00:00Z",
                "user": {
                    "id": 7,
                    "email": "user@example.com",
                    "display_name": "Z8 User"
                }
            }),
            "user@example.com",
        )
        .unwrap();
        let next = client
            .refresh(&old, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(next.access_token(), "access-next");
        assert_eq!(next.refresh_token(), Some("refresh-next"));
        assert_eq!(next.user.id.as_deref(), Some("7"));
        assert_eq!(next.user.display_name.as_deref(), Some("Z8 User"));
        assert_eq!(next.expires_at.as_deref(), Some("2026-10-01T00:00:00Z"));
    }

    #[tokio::test]
    async fn me_and_desktop_token_follow_launch_contract() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/desktop/token"))
            .and(body_json(json!({
                "client_id": "z8-launch",
                "code": "one-use-code",
                "code_verifier": "pkce-verifier",
                "redirect_uri": "http://127.0.0.1:4321/z8-launch/callback"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0,
                "data": session_value("desktop-access", "desktop-refresh", "desktop@example.com")
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/auth/me"))
            .and(header("authorization", "Bearer desktop-access"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0,
                "data": {"id": 7, "email": "desktop@example.com", "displayName": "Desktop User"}
            })))
            .mount(&server)
            .await;

        let client = AccountClient::new(format!("{}/v1", server.uri())).unwrap();
        let request = DesktopTokenRequest::new(
            "one-use-code",
            "pkce-verifier",
            "http://127.0.0.1:4321/z8-launch/callback",
        );
        assert!(!format!("{request:?}").contains("one-use-code"));
        assert!(!format!("{request:?}").contains("pkce-verifier"));
        let session = client
            .exchange_desktop_token(&request, &CancellationToken::new())
            .await
            .unwrap();
        let profile = client
            .me(&session, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(profile.id.as_deref(), Some("7"));
        assert_eq!(profile.display_name.as_deref(), Some("Desktop User"));
    }

    #[tokio::test]
    async fn desktop_token_rejects_access_only_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/desktop/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0,
                "data": {
                    "access_token": "desktop-access",
                    "user": {"id": 7, "email": "desktop@example.com"}
                }
            })))
            .mount(&server)
            .await;

        let client = AccountClient::new(format!("{}/v1", server.uri())).unwrap();
        let request = DesktopTokenRequest::new(
            "one-use-code",
            "pkce-verifier",
            "http://127.0.0.1:4321/z8-launch/callback",
        );
        let error = client
            .exchange_desktop_token(&request, &CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error.code(), AccountErrorCode::InvalidResponse);
        assert!(error.message().contains("refresh_token"));
    }

    #[tokio::test]
    async fn failed_login_does_not_replace_existing_session() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/login"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "code": 401,
                "reason": "INVALID_CREDENTIALS"
            })))
            .mount(&server)
            .await;
        let client = AccountClient::new(format!("{}/v1", server.uri())).unwrap();
        let store = AccountSessionStore::new();
        let existing = AccountSession::from_value(
            &session_value("keep-access", "keep-refresh", "old@example.com"),
            "old@example.com",
        )
        .unwrap();
        store.begin().commit(existing).unwrap();
        let operation = store.begin();
        let result = client
            .login(
                &LoginRequest::new("new@example.com", "wrong-pass"),
                &operation.cancellation(),
            )
            .await;
        assert!(result.is_err());
        assert_eq!(store.snapshot().unwrap().access_token(), "keep-access");
    }

    #[tokio::test]
    async fn cancelled_login_cannot_replace_existing_session() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/login"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(250))
                    .set_body_json(json!({
                        "code": 0,
                        "data": session_value("late-access", "late-refresh", "new@example.com")
                    })),
            )
            .mount(&server)
            .await;
        let client = AccountClient::new(format!("{}/v1", server.uri())).unwrap();
        let store = AccountSessionStore::new();
        let existing = AccountSession::from_value(
            &session_value("keep-access", "keep-refresh", "old@example.com"),
            "old@example.com",
        )
        .unwrap();
        store.begin().commit(existing).unwrap();
        let operation = store.begin();
        let cancellation = operation.cancellation();
        let task = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .login(
                        &LoginRequest::new("new@example.com", "secret123"),
                        &cancellation,
                    )
                    .await
            }
        });
        sleep(Duration::from_millis(30)).await;
        operation.cancel();
        let result = task.await.unwrap();
        assert_eq!(result.unwrap_err().code(), AccountErrorCode::Cancelled);
        assert_eq!(store.snapshot().unwrap().access_token(), "keep-access");
    }

    #[test]
    fn cancelled_or_superseded_commit_keeps_previous_session() {
        let store = AccountSessionStore::new();
        let existing = AccountSession::from_value(
            &session_value("keep-access", "keep-refresh", "old@example.com"),
            "old@example.com",
        )
        .unwrap();
        store.begin().commit(existing).unwrap();

        let cancelled_operation = store.begin();
        cancelled_operation.cancel();
        let late_session = AccountSession::from_value(
            &session_value("late-access", "late-refresh", "late@example.com"),
            "late@example.com",
        )
        .unwrap();
        assert_eq!(
            cancelled_operation
                .commit(late_session.clone())
                .unwrap_err()
                .code(),
            AccountErrorCode::Cancelled
        );
        assert_eq!(store.snapshot().unwrap().access_token(), "keep-access");

        let superseded = store.begin();
        let replacement = store.begin();
        assert_eq!(
            superseded.commit(late_session).unwrap_err().code(),
            AccountErrorCode::Cancelled
        );
        assert!(replacement
            .commit(
                AccountSession::from_value(
                    &session_value("new-access", "new-refresh", "new@example.com"),
                    "new@example.com",
                )
                .unwrap(),
            )
            .is_ok());
        assert_eq!(store.snapshot().unwrap().access_token(), "new-access");
    }

    #[test]
    fn maps_server_reason_to_stable_error_code() {
        let error = map_api_error(
            json!({"reason":"EMAIL_EXISTS"}).as_object().unwrap(),
            AccountError::new(AccountErrorCode::Conflict, "fallback"),
        );
        assert_eq!(error.code(), AccountErrorCode::EmailExists);
        assert_eq!(error.stable_code(), "account_email_exists");

        let nested = map_api_error(
            json!({"error":{"code":"EMAIL_EXISTS"},"message":"already used"})
                .as_object()
                .unwrap(),
            AccountError::new(AccountErrorCode::Conflict, "fallback"),
        );
        assert_eq!(nested.code(), AccountErrorCode::EmailExists);
        assert_eq!(nested.message(), "already used");
    }
}
