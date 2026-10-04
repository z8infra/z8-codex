export type ApiKeySummary = {
  id: string;
  name: string;
  status: string;
  secret: { masked: string; fingerprint: string };
  createdAt?: string | null;
  expiresAt?: string | null;
  selected: boolean;
};

export type AccountPayload = {
  status?: string;
  message?: string;
  authenticated: boolean;
  email?: string | null;
  keys: ApiKeySummary[];
  pendingTwoFactor?: boolean;
  pendingEmail?: string | null;
};

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null;

/**
 * Keep renderer state safe when a successful backend response is incomplete.
 * The Rust command currently serializes API keys as summaries, but this
 * boundary also protects the page from an older manager binary or a partial
 * account-service response. Invalid rows are omitted instead of making the
 * account page dereference an undefined secret or key list.
 */
function normalizeApiKey(value: unknown): ApiKeySummary | null {
  if (!isRecord(value) || typeof value.id !== "string" || !value.id.trim()) return null;
  if (!isRecord(value.secret) || typeof value.secret.masked !== "string" || typeof value.secret.fingerprint !== "string") return null;
  return {
    id: value.id,
    name: typeof value.name === "string" && value.name.trim() ? value.name : "Z8 API Key",
    status: typeof value.status === "string" ? value.status : "unknown",
    secret: { masked: value.secret.masked, fingerprint: value.secret.fingerprint },
    createdAt: typeof value.createdAt === "string" ? value.createdAt : null,
    expiresAt: typeof value.expiresAt === "string" ? value.expiresAt : null,
    selected: value.selected === true,
  };
}

/**
 * Normalizes every account payload before it enters React state.  Commands
 * other than redeem return a flattened account payload, so this helper is
 * intentionally separate from redeem response unwrapping.
 */
export function normalizeAccountPayload(payload: unknown): AccountPayload {
  const record = isRecord(payload) ? payload : null;
  const keys = Array.isArray(record?.keys)
    ? record.keys.map(normalizeApiKey).filter((key): key is ApiKeySummary => key !== null)
    : [];
  return {
    status: typeof record?.status === "string" ? record.status : undefined,
    message: typeof record?.message === "string" ? record.message : undefined,
    authenticated: record?.authenticated === true,
    email: typeof record?.email === "string" ? record.email : null,
    keys,
    pendingTwoFactor: record?.pendingTwoFactor === true,
    pendingEmail: typeof record?.pendingEmail === "string" ? record.pendingEmail : null,
  };
}

export function isUsableApiKey(key: Pick<ApiKeySummary, "status">): boolean {
  return key.status === "active" || key.status === "enabled" || key.status === "available";
}

/** Select the server-marked key, falling back to the first usable key. */
export function selectAccountKeyId(account: Pick<AccountPayload, "keys">): string {
  const keys = Array.isArray(account.keys) ? account.keys : [];
  return keys.find((key) => key.selected && isUsableApiKey(key))?.id
    ?? keys.find(isUsableApiKey)?.id
    ?? "";
}

export type AccountAuthSettings = {
  registrationEnabled: boolean;
  emailVerifyEnabled: boolean;
  invitationCodeEnabled: boolean;
  promoCodeEnabled: boolean;
  turnstileEnabled: boolean;
  turnstileSiteKey?: string | null;
  tencentCaptchaEnabled: boolean;
  tencentCaptchaAppId?: string | null;
  tencentCaptchaRegion?: string | null;
  aliyunCaptchaEnabled: boolean;
  aliyunCaptchaSceneId?: string | null;
  aliyunCaptchaPrefix?: string | null;
  aliyunCaptchaRegion?: string | null;
  loginAgreementEnabled: boolean;
  loginAgreementMode: string | null;
  loginAgreementRevision: string | null;
  loginAgreementUpdatedAt: string | null;
  loginAgreementDocuments: LoginAgreementDocument[];
};

export type LoginAgreementDocument = {
  id: string;
  title: string;
  contentMd: string;
};

export type LoginAgreement =
  | { kind: "unavailable"; reason: string }
  | { kind: "disabled" }
  | { kind: "required"; mode: "checkbox" | "modal"; documents: LoginAgreementDocument[]; identity: string };

/** Treat a malformed or incomplete legal contract as unavailable, never as consent-free. */
export function resolveLoginAgreement(settings: AccountAuthSettings | null | undefined): LoginAgreement {
  if (!settings || typeof settings.loginAgreementEnabled !== "boolean") {
    return { kind: "unavailable", reason: "账户协议设置暂不可用，请重试。" };
  }
  if (!settings.loginAgreementEnabled) return { kind: "disabled" };
  const documents = settings.loginAgreementDocuments;
  if (!Array.isArray(documents) || documents.length === 0 || documents.length > 8 ||
      documents.some((doc) => !doc || typeof doc.id !== "string" || !doc.id.trim() ||
        typeof doc.title !== "string" || !doc.title.trim() ||
        typeof doc.contentMd !== "string" || !doc.contentMd.trim())) {
    return { kind: "unavailable", reason: "账户协议内容不完整，请重试。" };
  }
  const encoder = new TextEncoder();
  const ids = new Set<string>();
  let totalBytes = 0;
  for (const doc of documents) {
    const bodyBytes = encoder.encode(doc.contentMd).byteLength;
    totalBytes += bodyBytes;
    if (ids.has(doc.id) || bodyBytes > 32 * 1024 || totalBytes > 128 * 1024) {
      return { kind: "unavailable", reason: "账户协议内容异常，请重试。" };
    }
    ids.add(doc.id);
  }
  if (settings.loginAgreementMode !== "checkbox" && settings.loginAgreementMode !== "modal") {
    return { kind: "unavailable", reason: "账户协议确认方式暂不支持，请重试。" };
  }
  // Include the document bodies so any changed terms invalidate in-memory consent,
  // even if an older account service omits or forgets to update its revision.
  const identity = JSON.stringify([
    settings.loginAgreementMode,
    settings.loginAgreementRevision,
    settings.loginAgreementUpdatedAt,
    documents.map(({ id, title, contentMd }) => [id, title, contentMd]),
  ]);
  return { kind: "required", mode: settings.loginAgreementMode, documents, identity };
}

export type AuthSettingsPayload = {
  settings: AccountAuthSettings;
};

export type CaptchaProvider =
  | { kind: "none" }
  | { kind: "invalid"; reason: string }
  | { kind: "turnstile"; siteKey: string }
  | { kind: "tencent"; appId: string; region?: string | null }
  | { kind: "aliyun"; sceneId: string; prefix: string; region?: string | null };

export type RedeemResult = Partial<AccountPayload> & {
  account?: Partial<AccountPayload> | null;
  receipt?: { message?: string | null; value?: number | null; newBalance?: number | null };
};

export type AccountCommandResult<T> = T & {
  status: string;
  message: string;
};

/** Extracts the account payload from the flattened redeem command response. */
export function normalizeRedeemAccount(result: RedeemResult): AccountPayload {
  const account = result.account ?? result;
  return normalizeAccountPayload({
    ...account,
    status: account.status ?? result.status,
    message: account.message ?? result.message,
  });
}

export function isSuccessfulAccountCommand(result: { status?: string } | null | undefined): boolean {
  return result?.status === "ok" || result?.status === "accepted";
}

export function authSettingsRequireCaptcha(settings: AccountAuthSettings | null | undefined): boolean {
  const provider = resolveCaptchaProvider(settings);
  return provider.kind !== "none";
}

/**
 * Selects the one server-configured captcha provider. The account API should
 * enable at most one provider; treating ambiguous or incomplete settings as
 * invalid prevents submitting a proof to the wrong verifier.
 */
export function resolveCaptchaProvider(settings: AccountAuthSettings | null | undefined): CaptchaProvider {
  if (!settings) return { kind: "none" };
  const enabled = [
    settings.turnstileEnabled ? "turnstile" : null,
    settings.tencentCaptchaEnabled ? "tencent" : null,
    settings.aliyunCaptchaEnabled ? "aliyun" : null,
  ].filter((provider): provider is string => provider !== null);
  if (enabled.length > 1) return { kind: "invalid", reason: "账户服务同时启用了多个安全验证组件。" };
  if (settings.turnstileEnabled) {
    const siteKey = settings.turnstileSiteKey?.trim();
    return siteKey ? { kind: "turnstile", siteKey } : { kind: "invalid", reason: "Cloudflare 安全验证配置不完整。" };
  }
  if (settings.tencentCaptchaEnabled) {
    const appId = settings.tencentCaptchaAppId?.trim();
    const region = settings.tencentCaptchaRegion?.trim() || null;
    if (!appId || (region !== null && region !== "cn" && region !== "intl")) return { kind: "invalid", reason: "腾讯安全验证配置不完整。" };
    return { kind: "tencent", appId, region };
  }
  if (settings.aliyunCaptchaEnabled) {
    const sceneId = settings.aliyunCaptchaSceneId?.trim();
    const prefix = settings.aliyunCaptchaPrefix?.trim();
    const region = settings.aliyunCaptchaRegion?.trim() || null;
    if (!sceneId || !prefix || (region !== null && region !== "cn" && region !== "sgp")) return { kind: "invalid", reason: "阿里云安全验证配置不完整。" };
    return { kind: "aliyun", sceneId, prefix, region };
  }
  return { kind: "none" };
}

export function canSubmitAuth(input: {
  accountLoading: boolean;
  busy: boolean;
  email: string;
  password: string;
  twoFactorCode: string;
  pendingTwoFactor: boolean;
  registerMode: boolean;
  registrationEnabled: boolean;
  emailVerificationRequired?: boolean;
  verifyCode?: string;
  invitationCodeRequired?: boolean;
  invitationCode?: string;
  captchaRequired: boolean;
  agreementBlocked?: boolean;
}): boolean {
  if (input.accountLoading || input.busy) return false;
  if (input.pendingTwoFactor) return input.twoFactorCode.trim().length > 0;
  if (input.captchaRequired || input.agreementBlocked) return false;
  if (!input.email.trim() || !input.password) return false;
  if (!input.registerMode || !input.registrationEnabled) return !input.registerMode;
  if (input.emailVerificationRequired && !input.verifyCode?.trim()) return false;
  return !input.invitationCodeRequired || Boolean(input.invitationCode?.trim());
}

const ACCOUNT_ERROR_COPY: Record<string, string> = {
  account_invalid_credentials: "邮箱或密码错误，请重试。",
  account_request_invalid: "提交内容无效，请检查后重试。",
  account_registration_invalid: "注册信息无效，请检查验证码和密码。",
  account_registration_disabled: "暂未开放注册，请直接登录。",
  account_email_exists: "邮箱已注册，请直接登录。",
  account_email_verify_required: "请先完成邮箱验证码验证。",
  account_verification_failed: "验证码发送失败，请重试。",
  account_captcha_failed: "安全验证失败，请稍后重试。",
  account_captcha_unavailable: "安全验证暂不可用，请稍后重试。",
  account_security_challenge: "账户连接被安全验证拦截，请稍后重试；持续出现请联系支持。",
  account_two_factor_required: "账户需要完成双因素验证，请输入二次验证码。",
  account_two_factor_invalid: "二次验证码错误或已过期，请重新登录。",
  account_session_expired: "登录已过期，请重新登录。",
  account_session_revoked: "登录已失效，请重新登录。",
  account_rate_limited: "操作过多，请稍后重试。",
  account_timeout: "账户服务超时，请检查网络后重试。",
  account_unavailable: "账户服务暂时不可用，请稍后重试。",
  account_invitation_required: "请输入邀请码。",
  account_invitation_invalid: "邀请码无效，请重试。",
  account_email_reserved: "该邮箱不允许注册，请更换。",
  account_email_suffix_not_allowed: "该邮箱后缀不允许注册。",
  account_email_domain_limit: "该邮箱域名注册已达上限。",
  account_registration_api_key_provision_failed: "账号已创建，但 API Key 创建失败，请直接登录。",
  account_registration_unavailable: "注册服务暂时不可用，请稍后重试。",
  account_invalid_response: "账户数据异常，请稍后重试。",
  account_login_cancelled: "已取消登录，请重试。",
  account_forbidden: "当前账户无法执行此操作。",
  account_conflict: "操作发生冲突，请稍后重试。",
};

const SAFE_COMMAND_MESSAGE_COPY: Record<string, string> = {
  "请先登录 Z8 账户": "请先登录 Z8 账户。",
  "找不到所选 API Key": "找不到所选 API Key，请刷新后重试。",
  "所选 API Key 当前不可用": "所选 API Key 当前不可用，请刷新后选择其他 Key。",
};

const CAPTCHA_ERROR_CODES = new Set([
  "account_captcha_failed",
  "account_captcha_unavailable",
  "account_security_challenge",
]);

/**
 * Maps the stable core error prefix to safe next-step copy. Unknown command
 * messages fall back to generic copy so server details can never echo a key or
 * token into the account page.
 */
export function accountFailureMessage(message: string, preserveExistingAccount = false): string {
  const normalized = message.trim();
  const code = normalized.split(":", 1)[0];
  const copy = ACCOUNT_ERROR_COPY[code] ?? SAFE_COMMAND_MESSAGE_COPY[normalized] ?? "账户操作未完成，请重试。";
  // Stable codes are safe to expose and make platform-specific failures
  // actionable when a user reports an issue from the desktop client.
  const hasStableCode = /^account_[a-z0-9_]+$/.test(code);
  const diagnosticCopy = hasStableCode ? `${copy} 错误代码：${code}` : copy;
  if (!preserveExistingAccount) return diagnosticCopy;
  return `${diagnosticCopy} 现有账户状态已保留。`;
}

/** Return the stable account error code without exposing server details. */
export function accountErrorCode(message: string): string {
  const normalized = message.trim();
  const prefix = normalized.split(":", 1)[0] ?? "";
  if (/^account_[a-z0-9_]+$/.test(prefix)) return prefix;
  // The user-facing copy keeps the stable code after the Chinese label so
  // the form can still associate a server failure with its inline field hint.
  return normalized.match(/错误代码：\s*(account_[a-z0-9_]+)/i)?.[1]?.toLowerCase() ?? "";
}

/**
 * A failed credential check does not invalidate the browser challenge. Only
 * challenge-specific failures should force a new proof and widget instance.
 */
export function shouldResetCaptchaAfterFailure(message: string): boolean {
  return CAPTCHA_ERROR_CODES.has(accountErrorCode(message));
}
