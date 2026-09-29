import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import {
  accountFailureMessage,
  authSettingsRequireCaptcha,
  canSubmitAuth,
  isSuccessfulAccountCommand,
  normalizeAccountPayload,
  normalizeRedeemAccount,
  resolveCaptchaProvider,
  resolveLoginAgreement,
  selectAccountKeyId,
} from "./account-flow";
import { canSubmitRedeem, completeAuthenticatedAccount, isProviderCheckHealthy, Z8AgreementDocuments, Z8SubmitForm } from "./Z8AccountPanel";

test("provider health accepts both native success statuses", () => {
  assert.equal(isProviderCheckHealthy({ status: "ok" }), true);
  assert.equal(isProviderCheckHealthy({ status: "healthy" }), true);
  assert.equal(isProviderCheckHealthy({ status: "failed" }), false);
  assert.equal(isProviderCheckHealthy(null), false);
});

test("native form keyboard submission obeys the authentication gate", () => {
  const common = {
    accountLoading: false, busy: false, email: "user@example.com", password: "password",
    twoFactorCode: "", pendingTwoFactor: false, registerMode: false,
    registrationEnabled: true, captchaRequired: false, agreementBlocked: false,
  };
  let actions = 0;
  for (const ready of [
    canSubmitAuth({ ...common, email: "" }),
    canSubmitAuth({ ...common, password: "" }),
    canSubmitAuth({ ...common, captchaRequired: true }), // security settings still loading
    canSubmitAuth({ ...common, busy: true }),
    canSubmitAuth({ ...common, agreementBlocked: true }),
  ]) {
    assert.equal(ready, false);
    const form = Z8SubmitForm({ ready, onAction: () => { actions += 1; }, className: "z8-account-form", children: "" });
    assert.equal(form.type, "form");
    let prevented = false;
    form.props.onSubmit({ preventDefault: () => { prevented = true; } });
    assert.equal(prevented, true);
    assert.equal(actions, 0);
  }
  const form = Z8SubmitForm({ ready: canSubmitAuth(common), onAction: () => { actions += 1; }, className: "z8-account-form", children: "" });
  form.props.onSubmit({ preventDefault: () => {} });
  assert.equal(actions, 1);
});

test("redeem form blocks Enter while busy or when the code is empty", () => {
  assert.equal(canSubmitRedeem(null, ""), false);
  assert.equal(canSubmitRedeem(null, "  "), false);
  assert.equal(canSubmitRedeem("z8_redeem", "CODE"), false);
  assert.equal(canSubmitRedeem(null, "CODE"), true);
  let actions = 0;
  const blocked = Z8SubmitForm({ ready: canSubmitRedeem("z8_redeem", "CODE"), onAction: () => { actions += 1; }, className: "form-row", children: "" });
  blocked.props.onSubmit({ preventDefault: () => {} });
  assert.equal(actions, 0);
  const allowed = Z8SubmitForm({ ready: canSubmitRedeem(null, "CODE"), onAction: () => { actions += 1; }, className: "form-row", children: "" });
  allowed.props.onSubmit({ preventDefault: () => {} });
  assert.equal(actions, 1);
});

test("normalizeRedeemAccount unwraps flattened redeem responses", () => {
  const account = normalizeRedeemAccount({
    status: "ok",
    message: "兑换成功",
    receipt: { value: 30 },
    account: {
      authenticated: true,
      email: "user@example.com",
      keys: [{ id: "k1", name: "Z8", status: "active", secret: { masked: "sk…1234", fingerprint: "fp" }, selected: true }],
    },
  });

  assert.equal(account.message, "兑换成功");
  assert.equal(account.keys[0]?.id, "k1");
});

test("normalizes incomplete account payloads before renderer state", () => {
  const account = normalizeAccountPayload({
    authenticated: true,
    email: "user@example.com",
    keys: [
      undefined,
      { id: "", name: "bad", status: "active", secret: { masked: "", fingerprint: "" } },
      { id: "k1", name: "Z8", status: "active", secret: { masked: "sk…1234", fingerprint: "fp" }, selected: true },
    ],
  });

  assert.equal(account.authenticated, true);
  assert.equal(account.keys.length, 1);
  assert.equal(account.keys[0]?.id, "k1");
});

test("redeem normalization remains safe when the nested account is absent", () => {
  const account = normalizeRedeemAccount({ status: "ok", message: "兑换成功", account: null });
  assert.equal(account.authenticated, false);
  assert.deepEqual(account.keys, []);
});

test("selects the server-marked usable key and falls back when it is unavailable", () => {
  const keys = [
    { id: "revoked", name: "revoked", status: "revoked", secret: { masked: "x", fingerprint: "x" }, selected: true },
    { id: "active", name: "active", status: "active", secret: { masked: "x", fingerprint: "x" }, selected: false },
  ];
  assert.equal(selectAccountKeyId({ keys }), "active");
  assert.equal(selectAccountKeyId({ keys: [] }), "");
});

test("failed account commands are not treated as state commits", () => {
  assert.equal(isSuccessfulAccountCommand({ status: "failed" }), false);
  assert.equal(isSuccessfulAccountCommand({ status: "ok" }), true);
  assert.equal(isSuccessfulAccountCommand({ status: "accepted" }), true);
});

test("account failures keep the existing account and explain unavailable steps", () => {
  assert.equal(
    accountFailureMessage("account_two_factor_required: 需要二次验证"),
    "账户需要完成双因素验证，请输入二次验证码。",
  );
  assert.equal(
    accountFailureMessage("account_invalid_credentials: invalid", true),
    "邮箱或密码错误，请重试。 现有账户状态已保留。",
  );
  assert.equal(accountFailureMessage("server detail [credential removed]"), "账户操作未完成，请重试。");
});

test("auth settings select one complete captcha provider", () => {
  assert.equal(authSettingsRequireCaptcha(undefined), false);
  const base = {
    registrationEnabled: true,
    emailVerifyEnabled: true,
    invitationCodeEnabled: false,
    promoCodeEnabled: false,
    turnstileEnabled: false,
    turnstileSiteKey: null,
    tencentCaptchaEnabled: false,
    tencentCaptchaAppId: null,
    aliyunCaptchaEnabled: false,
    aliyunCaptchaSceneId: null,
    aliyunCaptchaPrefix: null,
    loginAgreementEnabled: false,
    loginAgreementMode: null,
    loginAgreementRevision: null,
    loginAgreementUpdatedAt: null,
    loginAgreementDocuments: [],
  };
  assert.deepEqual(resolveCaptchaProvider({ ...base, turnstileEnabled: true, turnstileSiteKey: "site-key" }), { kind: "turnstile", siteKey: "site-key" });
  assert.deepEqual(resolveCaptchaProvider({ ...base, tencentCaptchaEnabled: true, tencentCaptchaAppId: "app-id" }), { kind: "tencent", appId: "app-id", region: null });
  assert.deepEqual(resolveCaptchaProvider({ ...base, aliyunCaptchaEnabled: true, aliyunCaptchaSceneId: "scene", aliyunCaptchaPrefix: "prefix" }), { kind: "aliyun", sceneId: "scene", prefix: "prefix", region: null });
  assert.equal(resolveCaptchaProvider({ ...base, turnstileEnabled: true, tencentCaptchaEnabled: true, turnstileSiteKey: "site-key", tencentCaptchaAppId: "app-id" }).kind, "invalid");
  assert.equal(resolveCaptchaProvider({ ...base, turnstileEnabled: true }).kind, "invalid");
  assert.equal(resolveCaptchaProvider({ ...base, tencentCaptchaEnabled: true, tencentCaptchaAppId: "app-id", tencentCaptchaRegion: "eu" }).kind, "invalid");
});

test("two-factor submission does not require the cleared password", () => {
  const common = {
    accountLoading: false,
    busy: false,
    email: "user@example.com",
    password: "",
    pendingTwoFactor: true,
    registerMode: false,
    registrationEnabled: false,
    captchaRequired: false,
  };
  assert.equal(canSubmitAuth({ ...common, twoFactorCode: "" }), false);
  assert.equal(canSubmitAuth({ ...common, twoFactorCode: "123456" }), true);
  assert.equal(canSubmitAuth({ ...common, twoFactorCode: "123456", busy: true }), false);
  assert.equal(canSubmitAuth({ ...common, captchaRequired: true, pendingTwoFactor: true, twoFactorCode: "123456" }), true);
  assert.equal(canSubmitAuth({ ...common, captchaRequired: false, pendingTwoFactor: true, twoFactorCode: "123456" }), true);
});

test("registration requires an enabled invitation code", () => {
  const common = {
    accountLoading: false,
    busy: false,
    email: "user@example.com",
    password: "correct horse battery staple",
    twoFactorCode: "",
    pendingTwoFactor: false,
    registerMode: true,
    registrationEnabled: true,
    captchaRequired: false,
    invitationCodeRequired: true,
  };
  assert.equal(canSubmitAuth({ ...common, invitationCode: "" }), false);
  assert.equal(canSubmitAuth({ ...common, invitationCode: " INVITE-2026 " }), true);
  assert.equal(canSubmitAuth({ ...common, invitationCodeRequired: false, invitationCode: "" }), true);
});

test("registration requires a nonempty email verification code only when configured", () => {
  const common = {
    accountLoading: false, busy: false, email: "user@example.com", password: "password",
    twoFactorCode: "", pendingTwoFactor: false, registerMode: true,
    registrationEnabled: true, captchaRequired: false,
  };
  assert.equal(canSubmitAuth({ ...common, emailVerificationRequired: true, verifyCode: "" }), false);
  assert.equal(canSubmitAuth({ ...common, emailVerificationRequired: true, verifyCode: "   " }), false);
  assert.equal(canSubmitAuth({ ...common, emailVerificationRequired: true, verifyCode: " 123456 " }), true);
  assert.equal(canSubmitAuth({ ...common, emailVerificationRequired: false, verifyCode: "" }), true);
  assert.equal(canSubmitAuth({ ...common, registerMode: false, emailVerificationRequired: true, verifyCode: "" }), true);
  assert.equal(canSubmitAuth({ ...common, pendingTwoFactor: true, twoFactorCode: "123456", emailVerificationRequired: true, verifyCode: "" }), true);
});

test("login and registration require explicit consent to the current legal documents", () => {
  const common = {
    accountLoading: false, busy: false, email: "user@example.com", password: "password",
    twoFactorCode: "", pendingTwoFactor: false, registerMode: false,
    registrationEnabled: true, captchaRequired: false,
  };
  assert.equal(canSubmitAuth({ ...common, agreementBlocked: true }), false);
  assert.equal(canSubmitAuth({ ...common, agreementBlocked: false }), true);
  assert.equal(canSubmitAuth({ ...common, registerMode: true, agreementBlocked: true }), false);
  assert.equal(canSubmitAuth({ ...common, registerMode: true, agreementBlocked: false }), true);
  // A previously initiated login may finish two-factor verification even if
  // public settings temporarily cannot be refreshed.
  assert.equal(canSubmitAuth({ ...common, pendingTwoFactor: true, password: "", twoFactorCode: "123456", agreementBlocked: true }), true);
});

test("legal agreement fails closed on missing settings or malformed documents", () => {
  const settings = {
    registrationEnabled: true, emailVerifyEnabled: true, invitationCodeEnabled: false,
    promoCodeEnabled: false, turnstileEnabled: false, tencentCaptchaEnabled: false,
    aliyunCaptchaEnabled: false, loginAgreementEnabled: true,
    loginAgreementMode: "checkbox", loginAgreementRevision: "v1",
    loginAgreementUpdatedAt: "2026-09-26",
    loginAgreementDocuments: [{ id: "terms", title: "服务条款", contentMd: "Terms text" }],
  };
  assert.equal(resolveLoginAgreement(null).kind, "unavailable");
  assert.equal(resolveLoginAgreement({ ...settings, loginAgreementDocuments: [] }).kind, "unavailable");
  assert.equal(resolveLoginAgreement({ ...settings, loginAgreementDocuments: [{ id: "terms", title: "", contentMd: "Terms text" }] }).kind, "unavailable");
  assert.equal(resolveLoginAgreement({ ...settings, loginAgreementDocuments: [settings.loginAgreementDocuments[0], settings.loginAgreementDocuments[0]] }).kind, "unavailable");
  assert.equal(resolveLoginAgreement({ ...settings, loginAgreementDocuments: [{ id: "terms", title: "服务条款", contentMd: "x".repeat(32 * 1024 + 1) }] }).kind, "unavailable");
  assert.equal(resolveLoginAgreement({ ...settings, loginAgreementMode: "unexpected" }).kind, "unavailable");
  assert.deepEqual(resolveLoginAgreement({ ...settings, loginAgreementEnabled: false }), { kind: "disabled" });
  const first = resolveLoginAgreement(settings);
  const changedRevision = resolveLoginAgreement({ ...settings, loginAgreementRevision: "v2" });
  const changedBody = resolveLoginAgreement({ ...settings, loginAgreementDocuments: [{ id: "terms", title: "服务条款", contentMd: "Changed terms" }] });
  assert.equal(first.kind, "required");
  assert.equal(changedRevision.kind, "required");
  assert.equal(changedBody.kind, "required");
  if (first.kind === "required" && changedRevision.kind === "required" && changedBody.kind === "required") {
    assert.notEqual(first.identity, changedRevision.identity);
    assert.notEqual(first.identity, changedBody.identity);
  }
});

test("agreement uses the four Z8 website policies and escapes unexpected service documents", () => {
  const html = renderToStaticMarkup(createElement(Z8AgreementDocuments, { documents: [
    { id: "terms", title: "模拟服务条款（无实际效力）", contentMd: "# Mock terms" },
    { id: "privacy", title: "模拟隐私说明（无实际效力）", contentMd: "# Mock privacy" },
    { id: "extra", title: "新增协议", contentMd: "<img src=x onerror=alert(1)>" },
  ] }));
  for (const [title, path] of [
    ["服务条款", "terms"],
    ["支持的国家和地区", "supported-regions"],
    ["隐私政策", "privacy-policy"],
    ["使用政策", "usage-policy"],
  ]) {
    assert.match(html, new RegExp(`href="https://z8\\.hk/legal/${path}"[^>]*>${title}</a>`));
  }
  assert.equal((html.match(/target="_blank" rel="noopener noreferrer"/g) ?? []).length, 4);
  assert.doesNotMatch(html, /模拟服务条款|模拟隐私说明|Mock terms|Mock privacy/);
  assert.match(html, /新增协议/);
  assert.match(html, /&lt;img src=x onerror=alert\(1\)&gt;/);
  assert.doesNotMatch(html, /<img /);
});

test("current public agreement IDs use website links without duplicate document panels", () => {
  const html = renderToStaticMarkup(createElement(Z8AgreementDocuments, { documents: [
    { id: "terms", title: "服务条款", contentMd: "# Current terms" },
    { id: "supported-regions", title: "支持的国家和地区", contentMd: "# Current regions" },
    { id: "privacy-policy", title: "隐私政策", contentMd: "# Current privacy" },
    { id: "usage-policy", title: "使用政策", contentMd: "# Current usage" },
  ] }));
  assert.equal((html.match(/href="https:\/\/z8\.hk\/legal\//g) ?? []).length, 4);
  assert.doesNotMatch(html, /<details>|Current regions|Current privacy|Current usage/);
});

test("two-factor and redeem only replace account state with complete snapshots", () => {
  const key = { id: "k1", name: "Z8", status: "active", secret: { masked: "sk…1234", fingerprint: "fp" }, selected: true };
  const twoFactor = completeAuthenticatedAccount({ status: "ok", authenticated: true, email: "user@example.com", keys: [key] });
  assert.equal(twoFactor?.keys[0]?.id, "k1");
  assert.equal(completeAuthenticatedAccount({ status: "ok", authenticated: true, email: "user@example.com", keys: undefined as never }), null);
  assert.equal(completeAuthenticatedAccount({ status: "ok", authenticated: true, email: null, keys: [key] }), null);
  assert.equal(completeAuthenticatedAccount({ status: "ok", authenticated: true, email: "user@example.com", keys: [{ id: "", name: "bad", status: "active", secret: key.secret, selected: true }] }), null);

  const redeemed = completeAuthenticatedAccount({ status: "ok", message: "兑换成功", account: { authenticated: true, email: "user@example.com", keys: [key] } });
  assert.equal(redeemed?.message, "兑换成功");
  assert.equal(redeemed?.keys[0]?.id, "k1");
  assert.equal(completeAuthenticatedAccount({ status: "ok", account: { authenticated: true, email: "user@example.com" } }), null);
  assert.equal(completeAuthenticatedAccount({ status: "ok", account: null }), null);
  // An authenticated account may legitimately have no API keys yet.
  assert.deepEqual(completeAuthenticatedAccount({ status: "ok", authenticated: true, email: "user@example.com", keys: [] })?.keys, []);
});
