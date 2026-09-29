//! Integration harness for the account module before it is wired into `lib.rs`.
//!
//! Keeping this path-based harness separate means the account slice can be
//! tested and reviewed independently. The main branch can later expose the
//! module from `codex-plus-core` without changing its implementation.
#[path = "../src/z8_account.rs"]
mod z8_account;

use serde_json::json;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn request_debug_output_redacts_credentials_and_codes() {
    let mut login = z8_account::LoginRequest::new("user@example.com", "password-secret");
    login.turnstile_token = Some("turnstile-secret".to_string());
    let login_debug = format!("{login:?}");
    assert!(!login_debug.contains("password-secret"));
    assert!(!login_debug.contains("turnstile-secret"));
    assert!(login_debug.contains("[redacted]"));

    let mut register = z8_account::RegisterRequest::new("user@example.com", "password-secret");
    register.promo_code = Some("promo-secret".to_string());
    let register_debug = format!("{register:?}");
    assert!(!register_debug.contains("password-secret"));
    assert!(!register_debug.contains("promo-secret"));

    let redeem_debug = format!(
        "{:?}",
        z8_account::RedeemRequest {
            code: "redeem-secret".to_string()
        }
    );
    assert!(!redeem_debug.contains("redeem-secret"));
}

#[tokio::test]
async fn public_settings_and_two_factor_follow_z8_launch_contract() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/settings/public"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "data": {
                "registration_enabled": true,
                "email_verify_enabled": true,
                "invitation_code_enabled": true,
                "promo_code_enabled": false,
                "turnstile_enabled": true,
                "turnstile_site_key": "site-key",
                "turnstile_secret_key": "must-not-leak",
                "tencent_captcha_enabled": true,
                "tencent_captcha_app_id": "app-id",
                "aliyun_captcha_enabled": false,
                "login_agreement_enabled": true,
                "login_agreement_mode": "checkbox",
                "login_agreement_revision": "2026-09-26",
                "login_agreement_updated_at": "2026-09-26T00:00:00Z",
                "login_agreement_documents": [{
                    "id": "terms",
                    "title": "服务条款",
                    "content_md": "# 服务条款\n\n请阅读后决定是否同意。"
                }]
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/login"))
        .and(body_json(json!({
            "email": "user@example.com",
            "password": "secret123",
            "turnstile_token": "turnstile-token"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "data": {
                "requires_2fa": true,
                "temp_token": "temporary-token",
                "user_email_masked": "u***@example.com"
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/login/2fa"))
        .and(body_json(json!({
            "temp_token": "temporary-token",
            "totp_code": "123456"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "data": {
                "access_token": "access-token",
                "refresh_token": "refresh-token",
                "user": {"email": "user@example.com"}
            }
        })))
        .mount(&server)
        .await;

    let client = z8_account::AccountClient::new(format!("{}/v1", server.uri())).unwrap();
    let settings = client
        .auth_settings(&z8_account::CancellationToken::new())
        .await
        .unwrap();
    assert!(settings.registration_enabled);
    assert_eq!(settings.turnstile_site_key.as_deref(), Some("site-key"));
    assert!(settings.login_agreement_enabled);
    assert_eq!(settings.login_agreement_mode.as_deref(), Some("checkbox"));
    assert_eq!(settings.login_agreement_documents[0].id, "terms");
    let rendered = serde_json::to_value(&settings).unwrap();
    assert_eq!(rendered["loginAgreementRevision"], "2026-09-26");
    assert_eq!(
        rendered["loginAgreementDocuments"][0]["contentMd"],
        "# 服务条款\n\n请阅读后决定是否同意。"
    );
    assert!(rendered.get("turnstile_secret_key").is_none());

    let mut request = z8_account::LoginRequest::new("user@example.com", "secret123");
    request.turnstile_token = Some("turnstile-token".to_string());
    let outcome = client
        .login_outcome(&request, &z8_account::CancellationToken::new())
        .await
        .unwrap();
    let challenge = match outcome {
        z8_account::LoginOutcome::TwoFactorRequired(challenge) => challenge,
        z8_account::LoginOutcome::Authenticated(_) => panic!("expected 2FA challenge"),
    };
    assert_eq!(challenge.temp_token(), "temporary-token");
    assert!(!format!("{challenge:?}").contains("temporary-token"));
    let session = client
        .complete_two_factor(&challenge, "123456", &z8_account::CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(session.access_token(), "access-token");
}

#[tokio::test]
async fn redeem_uses_type_alias_and_refresh_requires_rotated_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/redeem"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "data": {"message": "兑换成功", "type": "days", "value": 30.0}
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/refresh"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "data": {"access_token": "new-access"}
        })))
        .mount(&server)
        .await;
    let client = z8_account::AccountClient::new(format!("{}/v1", server.uri())).unwrap();
    let session = z8_account::AccountSession::from_persisted_value(&json!({
        "access_token": "old-access",
        "refresh_token": "old-refresh",
        "user": {"email": "user@example.com"}
    }))
    .unwrap();
    let receipt = client
        .redeem(&session, "Z8-123", &z8_account::CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(receipt.kind.as_deref(), Some("days"));
    assert_eq!(receipt.value, Some(30.0));
    let error = client
        .refresh(&session, &z8_account::CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code(), z8_account::AccountErrorCode::InvalidResponse);
}

#[tokio::test]
async fn refresh_never_adopts_a_different_user_from_rotated_tokens() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/refresh"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "data": {
                "access_token": "new-access",
                "refresh_token": "new-refresh",
                "user": {"id": 99, "email": "other@example.com"}
            }
        })))
        .mount(&server)
        .await;
    let client = z8_account::AccountClient::new(format!("{}/v1", server.uri())).unwrap();
    let old = z8_account::AccountSession::from_persisted_value(&json!({
        "access_token": "old-access",
        "refresh_token": "old-refresh",
        "user": {"id": 7, "email": "user@example.com", "display_name": "Original"}
    }))
    .unwrap();
    let error = client
        .refresh(&old, &z8_account::CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code(), z8_account::AccountErrorCode::InvalidResponse);
    assert_eq!(old.access_token(), "old-access");
    assert_eq!(old.user.email, "user@example.com");
}

#[test]
fn refresh_keeps_the_existing_profile_when_identity_matches() {
    let old = z8_account::AccountSession::from_persisted_value(&json!({
        "access_token": "old-access",
        "refresh_token": "old-refresh",
        "user": {"id": 7, "email": "user@example.com", "display_name": "Original"}
    }))
    .unwrap();
    let next = old
        .with_tokens_from(&json!({
            "access_token": "new-access",
            "refresh_token": "new-refresh",
            "user": {"id": 7, "email": "USER@example.com", "display_name": "Unexpected"}
        }))
        .unwrap();
    assert_eq!(next.user.email, "user@example.com");
    assert_eq!(next.user.display_name.as_deref(), Some("Original"));
    assert_eq!(next.access_token(), "new-access");
}

#[tokio::test]
async fn registration_rejects_embedded_key_with_wrong_owner_or_group() {
    for (key_user_id, group_id) in [(8, 3), (7, 0)] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/register"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0,
                "data": {
                    "access_token": "new-access",
                    "refresh_token": "new-refresh",
                    "user": {"id": 7, "email": "user@example.com"},
                    "api_key": {
                        "id": 21, "user_id": key_user_id, "group_id": group_id,
                        "key": "new-api-key", "status": "active"
                    }
                }
            })))
            .mount(&server)
            .await;
        let client = z8_account::AccountClient::new(format!("{}/v1", server.uri())).unwrap();
        let request = z8_account::RegisterRequest::new("user@example.com", "password123");
        let error = client
            .register(&request, &z8_account::CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error.code(), z8_account::AccountErrorCode::InvalidResponse);
    }
}

#[tokio::test]
async fn registration_accepts_an_embedded_key_with_matching_user_and_group() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/register"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "data": {
                "access_token": "new-access",
                "refresh_token": "new-refresh",
                "user": {"id": 7, "email": "user@example.com"},
                "api_key": {
                    "id": 21, "user_id": 7, "group_id": 3,
                    "key": "new-api-key", "status": "active"
                }
            }
        })))
        .mount(&server)
        .await;
    let client = z8_account::AccountClient::new(format!("{}/v1", server.uri())).unwrap();
    let request = z8_account::RegisterRequest::new("user@example.com", "password123");
    let session = client
        .register(&request, &z8_account::CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(session.user.email, "user@example.com");
}

#[tokio::test]
async fn redeem_rejects_a_negative_balance_in_success_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/redeem"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "data": {"message": "兑换成功", "type": "balance", "value": 10.0, "new_balance": -1.0}
        })))
        .mount(&server)
        .await;
    let client = z8_account::AccountClient::new(format!("{}/v1", server.uri())).unwrap();
    let session = z8_account::AccountSession::from_persisted_value(&json!({
        "access_token": "old-access",
        "refresh_token": "old-refresh",
        "user": {"email": "user@example.com"}
    }))
    .unwrap();
    let error = client
        .redeem(&session, "Z8-123", &z8_account::CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code(), z8_account::AccountErrorCode::InvalidResponse);
}
