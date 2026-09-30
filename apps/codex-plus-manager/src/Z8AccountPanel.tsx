import { useCallback, useEffect, useId, useMemo, useRef, useState, type FormEvent, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Activity,
  Check,
  CreditCard,
  ExternalLink,
  Eye,
  EyeOff,
  KeyRound,
  LogOut,
  RotateCcw,
  RefreshCw,
  Rocket,
  X,
  UserRound,
} from "lucide-react";
import {
  accountFailureMessage,
  shouldResetCaptchaAfterFailure,
  AccountCommandResult,
  AccountAuthSettings,
  AccountPayload,
  AuthSettingsPayload,
  canSubmitAuth,
  isUsableApiKey,
  isSuccessfulAccountCommand,
  LoginAgreementDocument,
  normalizeAccountPayload,
  normalizeRedeemAccount,
  resolveCaptchaProvider,
  resolveLoginAgreement,
  RedeemResult,
  selectAccountKeyId,
} from "./account-flow";
import { CaptchaChallenge, CaptchaProof } from "./CaptchaWidgets";

type ProviderCheck = {
  status: string;
  message: string;
  providerId?: string;
  endpoint?: string;
  model?: string;
  latencyMs?: number | null;
  error?: { code: string; message: string } | null;
};

export function isProviderCheckHealthy(check: Pick<ProviderCheck, "status"> | null | undefined): boolean {
  return check?.status === "ok" || check?.status === "healthy";
}

type UsageMetrics = {
  requests?: number | null;
  totalTokens?: number | null;
  cost?: number | null;
  actualCost?: number | null;
};

type UsageSnapshot = {
  mode: string;
  isValid?: boolean | null;
  isUnlimited: boolean;
  accountStatus?: string | null;
  planName?: string | null;
  unit?: string | null;
  remaining?: number | null;
  balance?: number | null;
  quota?: { limit?: number | null; used?: number | null; remaining?: number | null; unit?: string | null } | null;
  usage?: { today?: UsageMetrics | null; total?: UsageMetrics | null; averageDurationMs?: number | null; rpm?: number | null; tpm?: number | null } | null;
  observedAtMs: number;
};

const emptyAccount: AccountPayload = {
  status: "not_checked",
  message: "尚未读取账户状态",
  authenticated: false,
  email: null,
  keys: [],
};

const Z8_LEGAL_LINKS = [
  { id: "terms", title: "服务条款", url: "https://z8.hk/legal/terms" },
  { id: "supported-regions", title: "支持的国家和地区", url: "https://z8.hk/legal/supported-regions" },
  { id: "privacy-policy", title: "隐私政策", url: "https://z8.hk/legal/privacy-policy" },
  { id: "usage-policy", title: "使用政策", url: "https://z8.hk/legal/usage-policy" },
] as const;

// Keep the registration-code contract available while its controls stay private in the UI.
const SHOW_REGISTRATION_CODES = false;

const knownLegalIds = new Set<string>([
  ...Z8_LEGAL_LINKS.map(({ id }) => id),
  "regions", "privacy", "usage", // Older public-settings document IDs.
]);

/** Known Z8 policies open their public pages; unexpected service documents remain readable as escaped text. */
export function Z8AgreementDocuments({ documents, onOpenLink }: {
  documents: LoginAgreementDocument[];
  onOpenLink?: (url: string) => void;
}) {
  const additionalDocuments = documents.filter(({ id }) => !knownLegalIds.has(id));
  return (
    <>
      <span className="z8-agreement-links">
        {Z8_LEGAL_LINKS.map((link, index) => (
          <span key={link.id}>
            <a href={link.url} target="_blank" rel="noopener noreferrer"
              onClick={onOpenLink ? (event) => { event.preventDefault(); onOpenLink(link.url); } : undefined}>
              {link.title}
            </a>
            {index < Z8_LEGAL_LINKS.length - 1 ? "、" : null}
          </span>
        ))}
      </span>
      {additionalDocuments.length > 0 ? (
        <div className="z8-agreement-documents">
          {additionalDocuments.map((doc) => (
            <details key={doc.id}>
              <summary>{doc.title}</summary>
              <pre>{doc.contentMd}</pre>
            </details>
          ))}
        </div>
      ) : null}
    </>
  );
}

/** Native form Enter handling uses the same gate as the visible submit button. */
export function Z8SubmitForm({ ready, onAction, className, children }: {
  ready: boolean;
  onAction: () => void;
  className: string;
  children: ReactNode;
}) {
  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (ready) onAction();
  };
  return <form className={className} onSubmit={submit}>{children}</form>;
}

export function canSubmitRedeem(busy: string | null, code: string): boolean {
  return busy === null && code.trim().length > 0;
}

/** A successful command must carry a complete snapshot before replacing local account state. */
export function completeAuthenticatedAccount(result: AccountPayload | RedeemResult): AccountPayload | null {
  const raw = "account" in result ? result.account : result;
  if (!raw || raw.authenticated !== true || typeof raw.email !== "string" || !raw.email.trim() || !Array.isArray(raw.keys)) {
    return null;
  }
  const next = "account" in result ? normalizeRedeemAccount(result) : normalizeAccountPayload(result);
  return next.keys.length === raw.keys.length ? next : null;
}

function formatUsageNumber(value: number | null | undefined): string {
  return typeof value === "number" && Number.isFinite(value)
    ? new Intl.NumberFormat("zh-CN", { maximumFractionDigits: 0 }).format(value)
    : "—";
}

function formatUsageAmount(value: number | null | undefined, unit?: string | null): string {
  if (typeof value !== "number" || !Number.isFinite(value)) return "—";
  const amount = value.toFixed(2);
  return unit?.toUpperCase() === "USD" ? `$${amount}` : `${amount}${unit ? ` ${unit}` : ""}`;
}

type LaunchState = "idle" | "starting" | "started" | "failed";

const LAUNCH_TIMEOUT_MS = 45_000;

export function Z8AccountPanel({ onLaunch, onReset, onAuthenticated, onClose }: {
  /** Resolves true (or any non-false value) once the parent launch flow completes. */
  onLaunch?: () => boolean | void | Promise<boolean | void>;
  onReset?: () => Promise<boolean>;
  onAuthenticated?: () => void;
  onClose?: () => void;
}) {
  const fieldId = useId();
  const [account, setAccount] = useState<AccountPayload>(emptyAccount);
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [showPassword, setShowPassword] = useState(false);
  const [confirmPassword, setConfirmPassword] = useState("");
  const [showConfirmPassword, setShowConfirmPassword] = useState(false);
  const [redeemCode, setRedeemCode] = useState("");
  const [invitationCode, setInvitationCode] = useState("");
  const [promoCode, setPromoCode] = useState("");
  const [registerMode, setRegisterMode] = useState(false);
  const [verifyCode, setVerifyCode] = useState("");
  const [verificationDialogOpen, setVerificationDialogOpen] = useState(false);
  const [verificationSent, setVerificationSent] = useState(false);
  const [verificationEmail, setVerificationEmail] = useState("");
  const [verificationCooldown, setVerificationCooldown] = useState(0);
  const verificationAvailableAt = useRef(0);
  const [twoFactorCode, setTwoFactorCode] = useState("");
  const [pendingTwoFactor, setPendingTwoFactor] = useState(false);
  const [pendingEmail, setPendingEmail] = useState<string | null>(null);
  const [authSettings, setAuthSettings] = useState<AccountAuthSettings | null>(null);
  const [authSettingsLoading, setAuthSettingsLoading] = useState(true);
  const [authSettingsError, setAuthSettingsError] = useState("");
  const [acceptedAgreementIdentity, setAcceptedAgreementIdentity] = useState<string | null>(null);
  const [selectedKey, setSelectedKey] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [accountLoading, setAccountLoading] = useState(true);
  const [notice, setNotice] = useState("");
  const [providerCheck, setProviderCheck] = useState<ProviderCheck | null>(null);
  const [usage, setUsage] = useState<UsageSnapshot | null>(null);
  const [usageLoading, setUsageLoading] = useState(false);
  const [usageError, setUsageError] = useState("");
  const [providerAppliedKey, setProviderAppliedKey] = useState<string | null>(null);
  const [captchaProof, setCaptchaProof] = useState<CaptchaProof | null>(null);
  const [captchaResetNonce, setCaptchaResetNonce] = useState(0);
  const [launchState, setLaunchState] = useState<LaunchState>("idle");
  const activeRuns = useRef(new Map<string, number>());
  const usageRequestId = useRef(0);
  const usageInFlightKey = useRef<string | null>(null);
  const launchStateRef = useRef<LaunchState>("idle");
  const launchRequestId = useRef(0);
  const launchTimeoutRef = useRef<number | null>(null);
  const agreementDialogRef = useRef<HTMLDialogElement>(null);
  const redeemDialogRef = useRef<HTMLDialogElement>(null);
  const redeemTriggerRef = useRef<HTMLButtonElement>(null);
  const verificationDialogRef = useRef<HTMLDialogElement>(null);

  const captchaProvider = resolveCaptchaProvider(authSettings);
  const captchaRequired = captchaProvider.kind !== "none";
  const captchaConfigurationInvalid = captchaProvider.kind === "invalid";
  const captchaSettingsReady = !authSettingsLoading && authSettings !== null;
  const captchaReady = captchaSettingsReady && (!captchaRequired || Boolean(captchaProof && Object.keys(captchaProof).length > 0));
  const captchaWidgetProvider = captchaProvider.kind === "none" || captchaProvider.kind === "invalid" ? null : captchaProvider;
  const captchaBlocksAuthSubmit = !pendingTwoFactor && (!captchaReady || captchaConfigurationInvalid);
  const registrationEnabled = authSettings?.registrationEnabled === true;
  const emailVerificationEnabled = authSettings?.emailVerifyEnabled === true;
  const verificationStepActive = verificationDialogOpen && registerMode && emailVerificationEnabled;
  const normalizedEmail = email.trim().toLowerCase();
  const verificationSentForCurrentEmail = verificationSent && normalizedEmail.length > 0 && verificationEmail === normalizedEmail;
  const invitationCodeRequired = SHOW_REGISTRATION_CODES && registerMode && authSettings?.invitationCodeEnabled === true;
  const agreement = useMemo(() => resolveLoginAgreement(authSettings), [authSettings]);
  const agreementBlocked = agreement.kind === "unavailable" ||
    (agreement.kind === "required" && acceptedAgreementIdentity !== agreement.identity);
  const passwordConfirmationReady = !registerMode || (password.length > 0 && password === confirmPassword);
  const authSubmitReady = canSubmitAuth({
    accountLoading, busy: busy !== null, email, password, twoFactorCode,
    pendingTwoFactor, registerMode, registrationEnabled, invitationCodeRequired,
    invitationCode, emailVerificationRequired: false, verifyCode: "",
    // Once a code has been sent, the primary action only reopens the
    // verification dialog and must not require a second captcha proof.
    captchaRequired: registerMode && emailVerificationEnabled && verificationSentForCurrentEmail
      ? false
      : captchaBlocksAuthSubmit,
    agreementBlocked,
  }) && passwordConfirmationReady;
  const verificationSubmitReady = verificationStepActive && canSubmitAuth({
    accountLoading, busy: busy !== null, email, password, twoFactorCode,
    pendingTwoFactor, registerMode, registrationEnabled, invitationCodeRequired,
    invitationCode, emailVerificationRequired: true, verifyCode,
    captchaRequired: captchaBlocksAuthSubmit, agreementBlocked,
  }) && password.length > 0 && password === confirmPassword;
  const redeemSubmitReady = canSubmitRedeem(busy, redeemCode);
  const handleCaptchaProof = useCallback((proof: CaptchaProof | null) => {
    setCaptchaProof(proof);
  }, []);

  const resetCaptcha = useCallback(() => {
    setCaptchaProof(null);
    setCaptchaResetNonce((value) => value + 1);
  }, []);

  const usableKeys = useMemo(
    () => account.keys.filter(isUsableApiKey),
    [account.keys],
  );
  const openLegalLink = async (url: string) => {
    try {
      const result = await invoke<{ status: string; message: string }>("open_external_url", { url });
      if (result.status !== "ok") setNotice(result.message || "无法打开协议页面，请检查系统浏览器。");
    } catch {
      setNotice("无法打开协议页面，请检查系统浏览器。");
    }
  };

  const openPurchasePage = async () => {
    try {
      const result = await invoke<{ status: string; message: string }>("open_external_url", { url: "https://wzyp.cn/shop/UYFEXWJF" });
      if (result.status !== "ok") setNotice(result.message || "无法打开购买页面，请检查系统浏览器。");
    } catch {
      setNotice("无法打开购买页面，请检查系统浏览器。");
    }
  };

  const agreementDocumentList = agreement.kind === "required" ? (
    <Z8AgreementDocuments documents={agreement.documents} onOpenLink={(url) => void openLegalLink(url)} />
  ) : null;

  const selectKey = (next: AccountPayload, preferredKey = selectedKey) => {
    const preferred = preferredKey && next.keys.some((key) => key.id === preferredKey && isUsableApiKey(key))
      ? preferredKey
      : selectAccountKeyId(next);
    setSelectedKey(preferred);
  };

  const run = async <T,>(command: string, args?: Record<string, unknown>) => {
    activeRuns.current.set(command, (activeRuns.current.get(command) ?? 0) + 1);
    setBusy((current) => current ?? command);
    setNotice("");
    try {
      const result = await invoke<T>(command, args);
      return result;
    } catch (error) {
      setNotice(accountFailureMessage(error instanceof Error ? error.message : String(error), account.authenticated));
      return null;
    } finally {
      const remaining = (activeRuns.current.get(command) ?? 1) - 1;
      if (remaining > 0) {
        activeRuns.current.set(command, remaining);
      } else {
        activeRuns.current.delete(command);
      }
      setBusy((current) => {
        if (current !== command) return current;
        return activeRuns.current.keys().next().value ?? null;
      });
    }
  };

  const commitAccount = (payload: Partial<AccountPayload> | null | undefined, preferredKey = selectedKey): AccountPayload => {
    const next = normalizeAccountPayload(payload);
    setAccount(next);
    selectKey(next, preferredKey);
    setProviderCheck(null);
    setProviderAppliedKey(null);
    if (next.authenticated) onAuthenticated?.();
    return next;
  };

  const loadAuthSettings = async () => {
    setAuthSettingsLoading(true);
    setAuthSettings(null);
    setAuthSettingsError("");
    try {
      const result = await run<AccountCommandResult<AuthSettingsPayload>>("z8_auth_settings");
      if (!result || !isSuccessfulAccountCommand(result) || !result.settings) {
        setAuthSettingsError("账户安全与协议设置暂不可用，请重试后再登录或注册。");
        if (result) setNotice(accountFailureMessage(result.message, account.authenticated));
        return;
      }
      setAuthSettings(result.settings);
      if (!result.settings.registrationEnabled) setRegisterMode(false);
    } finally {
      setAuthSettingsLoading(false);
    }
  };

  const loadAccount = async () => {
    setAccountLoading(true);
    try {
      const result = await run<AccountCommandResult<AccountPayload>>("z8_account_status");
      if (!result) return;
      if (!isSuccessfulAccountCommand(result)) {
        setNotice(accountFailureMessage(result.message, account.authenticated));
        return;
      }
      const next = commitAccount(result);
      if (result.email) setEmail(result.email);
      setPendingTwoFactor(next.pendingTwoFactor === true);
      setPendingEmail(next.pendingEmail ?? null);
      const keyId = selectAccountKeyId(next);
      // The local account snapshot is enough to render the account page. Applying
      // the provider also refreshes keys, performs a health check, and fetches
      // usage; keeping that network chain inside the initial loading state made
      // a slow upstream leave the whole page stuck on "正在读取账户".
      if (next.authenticated && keyId) {
        // The selected-key effect starts usage loading immediately; the
        // background provider sync should not create a second usage request.
        void handleApply(keyId, false);
      }
    } finally {
      setAccountLoading(false);
    }
  };

  const loadUsage = async (keyId = selectedKey, authenticated = account.authenticated) => {
    if (!keyId || !authenticated) {
      usageRequestId.current += 1;
      usageInFlightKey.current = null;
      setUsage(null);
      setUsageError("");
      setUsageLoading(false);
      return;
    }
    // Account startup and provider application can both notice the selected
    // key. Reuse the first request instead of letting a later duplicate make
    // the earlier successful response stale.
    if (usageInFlightKey.current === keyId) return;
    const requestId = ++usageRequestId.current;
    usageInFlightKey.current = keyId;
    setUsageLoading(true);
    setUsageError("");
    try {
      const result = await run<AccountCommandResult<UsageSnapshot>>("z8_usage", { keyId });
      if (requestId !== usageRequestId.current) return;
      if (!result || !isSuccessfulAccountCommand(result)) {
        setUsage(null);
        // Keep the backend's safe, classified message so a failed upstream
        // request is distinguishable from a real zero balance.
        setUsageError(result?.message?.trim() || "余额暂不可用，请稍后重试。");
      } else {
        setUsage(result);
      }
    } finally {
      if (usageInFlightKey.current === keyId) {
        usageInFlightKey.current = null;
        if (requestId === usageRequestId.current) setUsageLoading(false);
      }
    }
  };

  useEffect(() => {
    void loadAccount();
    void loadAuthSettings();
  }, []);

  useEffect(() => {
    if (account.authenticated && selectedKey) void loadUsage(selectedKey);
    else {
      setUsage(null);
      setUsageError("");
    }
  }, [account.authenticated, selectedKey]);

  useEffect(() => {
    if (verificationCooldown <= 0) return;
    const timer = window.setTimeout(() => {
      setVerificationCooldown(Math.max(0, Math.ceil((verificationAvailableAt.current - Date.now()) / 1000)));
    }, 1000);
    return () => window.clearTimeout(timer);
  }, [verificationCooldown]);

  useEffect(() => {
    const dialog = verificationDialogRef.current;
    if (!dialog) return;
    if (verificationDialogOpen && !dialog.open) {
      dialog.showModal();
    } else if (!verificationDialogOpen && dialog.open) {
      dialog.close();
    }
  }, [verificationDialogOpen]);

  const handleAuth = async () => {
    if (agreementBlocked) {
      setNotice(agreement.kind === "unavailable" ? agreement.reason : "请先阅读并同意账户协议。");
      return;
    }
    if (registerMode && !registrationEnabled) {
      setNotice("当前暂未开放注册，请直接登录。");
      return;
    }
    if (registerMode && password !== confirmPassword) {
      setNotice("两次输入的密码不一致，请重新确认。");
      return;
    }
    if (invitationCodeRequired && !invitationCode.trim()) {
      setNotice("请输入邀请码。");
      return;
    }
    if (registerMode && emailVerificationEnabled && verificationSentForCurrentEmail && !verificationDialogOpen) {
      setVerificationDialogOpen(true);
      return;
    }
    if (captchaProvider.kind === "invalid") {
      setNotice(captchaProvider.reason);
      return;
    }
    if (!captchaReady && !pendingTwoFactor) {
      setNotice("请先完成安全验证。");
      return;
    }
    if (registerMode && emailVerificationEnabled && !verificationDialogOpen) {
      await sendVerificationCode();
      return;
    }
    if (registerMode && emailVerificationEnabled && !verifyCode.trim()) {
      setNotice("请输入邮箱验证码。");
      return;
    }
    const command = registerMode ? "z8_register" : "z8_login";
    const proof = captchaProof ?? {};
    const args = registerMode
      ? { input: {
          email,
          password,
          verifyCode: emailVerificationEnabled ? verifyCode || null : null,
          invitationCode: SHOW_REGISTRATION_CODES ? invitationCode.trim() || null : null,
          promoCode: SHOW_REGISTRATION_CODES && authSettings?.promoCodeEnabled ? promoCode.trim() || null : null,
          ...proof,
        } }
      : { input: { email, password, ...proof } };
    const result = await run<AccountCommandResult<AccountPayload>>(command, args);
    if (!result) return;
    if (!isSuccessfulAccountCommand(result)) {
      setNotice(accountFailureMessage(result.message, account.authenticated));
      if (shouldResetCaptchaAfterFailure(result.message)) resetCaptcha();
      return;
    }
    resetCaptcha();
    setPassword("");
    const next = commitAccount(result);
    setPendingTwoFactor(next.pendingTwoFactor === true);
    setPendingEmail(next.pendingEmail ?? null);
    if (next.pendingTwoFactor) setTwoFactorCode("");
    setNotice(result.message);
    setVerifyCode("");
    setConfirmPassword("");
    setVerificationDialogOpen(false);
    setVerificationSent(false);
    setVerificationEmail("");
    setVerificationCooldown(0);
    verificationAvailableAt.current = 0;
    if (next.authenticated) {
      const keyId = selectAccountKeyId(next);
      if (keyId) await handleApply(keyId);
    }
  };

  const handleTwoFactor = async () => {
    if (!twoFactorCode.trim()) return;
    const result = await run<AccountCommandResult<AccountPayload>>("z8_complete_two_factor", { input: { code: twoFactorCode } });
    if (!result) return;
    if (!isSuccessfulAccountCommand(result)) { setNotice(accountFailureMessage(result.message, account.authenticated)); return; }
    const next = completeAuthenticatedAccount(result);
    if (!next) {
      setNotice("二次验证结果缺少完整账户状态，待验证状态已保留，请重试。");
      return;
    }
    commitAccount(next);
    setPendingTwoFactor(false); setPendingEmail(null); setTwoFactorCode(""); setNotice(next.message ?? result.message);
    const keyId = selectAccountKeyId(next);
    if (keyId) await handleApply(keyId);
  };

  const cancelLogin = async () => {
    const result = await run<AccountCommandResult<AccountPayload>>("z8_cancel_login");
    if (!result) return;
    if (!isSuccessfulAccountCommand(result)) {
      setNotice(accountFailureMessage(result.message, account.authenticated));
      return;
    }
    setPendingTwoFactor(false); setPendingEmail(null); setTwoFactorCode(""); setNotice(result.message);
  };

  const sendVerificationCode = async () => {
    if (!emailVerificationEnabled) {
      setNotice("当前注册设置不需要邮箱验证码。");
      return;
    }
    if (captchaProvider.kind === "invalid") {
      setNotice(captchaProvider.reason);
      return;
    }
    if (verificationCooldown > 0 && verificationSentForCurrentEmail) {
      setVerificationDialogOpen(true);
      return;
    }
    if (!captchaReady) {
      setNotice("请先完成安全验证。");
      return;
    }
    const requestedEmail = email.trim();
    const result = await run<AccountCommandResult<{ email?: string | null; sent: boolean }>>("z8_send_verification_code", { input: { email: requestedEmail, ...(captchaProof ?? {}) } });
    if (!result) return;
    if (isSuccessfulAccountCommand(result)) {
      resetCaptcha();
      setVerificationSent(true);
      setVerificationEmail(requestedEmail.toLowerCase());
      verificationAvailableAt.current = Date.now() + 60_000;
      setVerificationCooldown(60);
      setVerifyCode("");
      setVerificationDialogOpen(true);
      setNotice(result.message || "验证码已发送，请查收邮箱。");
    } else {
      setNotice(accountFailureMessage(result.message, account.authenticated));
      if (shouldResetCaptchaAfterFailure(result.message)) resetCaptcha();
    }
  };

  const handleRedeem = async () => {
    const result = await run<AccountCommandResult<RedeemResult>>("z8_redeem", { code: redeemCode });
    if (!result) return;
    if (!isSuccessfulAccountCommand(result)) {
      setNotice(accountFailureMessage(result.message, account.authenticated));
      return;
    }
    const next = completeAuthenticatedAccount(result);
    if (!next) {
      setNotice("兑换结果缺少完整账户状态，现有账户状态已保留，请刷新后重试。");
      return;
    }
    commitAccount(next);
    setRedeemCode("");
    redeemDialogRef.current?.close();
    setNotice(result.receipt?.message || result.message);
    const keyId = selectAccountKeyId(next);
    if (keyId) await handleApply(keyId);
  };

  const handleCheck = async (keyId = selectedKey): Promise<boolean> => {
    if (!keyId) {
      setNotice("请选择可用的 Z8 API Key。");
      return false;
    }
    const result = await run<ProviderCheck>("z8_check_provider", { keyId });
    if (!result) return false;
    setProviderCheck(result);
    setNotice(result.message);
    return result.status === "ok" || result.status === "healthy";
  };

  const handleApply = async (keyId = selectedKey, refreshUsage = true): Promise<boolean> => {
    if (!keyId) return false;
    const result = await run<AccountCommandResult<AccountPayload>>("z8_apply_key", { keyId });
    if (result && isSuccessfulAccountCommand(result)) {
      commitAccount(result, keyId);
      setProviderAppliedKey(keyId);
      const usagePromise = refreshUsage ? loadUsage(keyId, true) : Promise.resolve();
      const healthyPromise = handleCheck(keyId);
      const [healthy] = await Promise.all([healthyPromise, usagePromise]);
      setNotice(healthy ? "Z8 Provider 已配置且可用" : "Z8 Provider 已写入，但健康检查未通过");
      return healthy;
    } else if (result) {
      setNotice(accountFailureMessage(result.message, account.authenticated));
    }
    return false;
  };

  const handleReset = async () => {
    if (!onReset || busy !== null) return;
    if (!window.confirm("恢复 Z8 默认供应商配置？这会覆盖你对 Z8 供应商字段的修改。")) return;
    setProviderCheck(null);
    setProviderAppliedKey(null);
    const reset = await onReset();
    if (!reset) return;
    const keyId = selectedKey || selectAccountKeyId(account);
    if (keyId) {
      setProviderAppliedKey(keyId);
      await handleCheck(keyId);
      await loadUsage(keyId, true);
    }
  };

  const refreshAccount = async (command: "z8_refresh_session" | "z8_refresh_keys") => {
    const result = await run<AccountCommandResult<AccountPayload>>(command);
    if (!result) return;
    if (!isSuccessfulAccountCommand(result)) {
      setNotice(accountFailureMessage(result.message, account.authenticated));
      return;
    }
    commitAccount(result);
    setNotice(result.message);
    const keyId = selectAccountKeyId(result);
    if (keyId) await handleApply(keyId);
  };

  const logout = async () => {
    const result = await run<AccountCommandResult<AccountPayload>>("z8_logout");
    if (!result) return;
    if (!isSuccessfulAccountCommand(result)) {
      setNotice(accountFailureMessage(result.message, account.authenticated));
      return;
    }
    commitAccount(result);
    setUsage(null);
    setUsageError("");
    setProviderAppliedKey(null);
    resetLaunchState();
    setNotice(result.message);
  };

  const authModal = accountLoading || !account.authenticated;
  const providerHealthy = isProviderCheckHealthy(providerCheck);
  const authTitleId = `${fieldId}-auth-title`;

  const updateLaunchState = (next: LaunchState) => {
    launchStateRef.current = next;
    setLaunchState(next);
  };

  const clearLaunchTimeout = () => {
    if (launchTimeoutRef.current === null) return;
    window.clearTimeout(launchTimeoutRef.current);
    launchTimeoutRef.current = null;
  };

  const resetLaunchState = () => {
    launchRequestId.current += 1;
    clearLaunchTimeout();
    updateLaunchState("idle");
  };

  useEffect(() => () => {
    launchRequestId.current += 1;
    clearLaunchTimeout();
  }, []);

  const handleLaunch = async () => {
    // Keep the guard in a ref as well as state so a second click in the same
    // event loop cannot enqueue another launch before React re-renders.
    if (!onLaunch || launchStateRef.current === "starting") return;
    const requestId = ++launchRequestId.current;
    clearLaunchTimeout();
    updateLaunchState("starting");
    setNotice("");
    launchTimeoutRef.current = window.setTimeout(() => {
      if (requestId !== launchRequestId.current || launchStateRef.current !== "starting") return;
      launchTimeoutRef.current = null;
      updateLaunchState("failed");
      setNotice("Codex 启动时间较长，可能仍在后台启动。请确认没有重复打开后再重试。");
    }, LAUNCH_TIMEOUT_MS);
    try {
      const result = await onLaunch();
      if (requestId !== launchRequestId.current) return;
      clearLaunchTimeout();
      if (result === false) {
        updateLaunchState("failed");
        setNotice("Codex 启动失败，请检查安装状态后重试。");
      } else {
        updateLaunchState("started");
      }
    } catch (error) {
      if (requestId !== launchRequestId.current) return;
      clearLaunchTimeout();
      updateLaunchState("failed");
      setNotice(error instanceof Error && error.message ? error.message : "Codex 启动失败，请稍后重试。");
    }
  };

  const launchButtonDisabled = !onLaunch
    || providerAppliedKey !== selectedKey
    || !providerHealthy
    || busy !== null
    || launchState === "starting";
  const launchButtonLabel = launchState === "starting"
    ? "启动中…"
    : launchState === "started"
      ? "打开 Codex"
      : launchState === "failed"
        ? "重试启动"
        : "启动 Codex";

  return (
    <div className={`z8-account-page${authModal ? " z8-account-auth-page" : ""}`}>
      <div className={authModal ? "z8-auth-stage" : "z8-account-panel-stage"}>
      <Card
        aria-labelledby={authModal ? authTitleId : undefined}
        aria-modal={authModal ? true : undefined}
        className={`panel${authModal ? " z8-auth-modal" : ""}`}
        role={authModal ? "dialog" : undefined}
      >
        {authModal ? <CardHeader className="panel-head z8-auth-modal-header">
          <div className="z8-auth-title-group">
            <span className="z8-auth-eyebrow">Z8 账户</span>
            <CardTitle id={authTitleId}>{accountLoading ? "正在读取 Z8 账户" : pendingTwoFactor ? "验证 Z8 账户" : registerMode ? "创建 Z8 账户" : "登录 Z8 账户"}</CardTitle>
          </div>
          <div className="z8-auth-header-actions">
            {onClose ? <Button type="button" variant="outline" size="icon" className="z8-auth-close" onClick={onClose} aria-label="关闭账户登录"><X aria-hidden="true" /></Button> : null}
          </div>
        </CardHeader> : null}
        <CardContent className={`z8-account-content${authModal ? " z8-auth-modal-content" : ""}`}>
          {accountLoading ? <p className="field-hint" role="status">正在读取账户状态，请稍候。</p> : !account.authenticated ? (
            <Z8SubmitForm className="z8-account-form z8-auth-form" ready={authSubmitReady} onAction={() => void (pendingTwoFactor ? handleTwoFactor() : handleAuth())}>
              {authSettingsError ? <div className="z8-auth-status" role="alert"><span>{authSettingsError}</span><Button type="button" variant="secondary" disabled={busy !== null} onClick={() => void loadAuthSettings()}>重试</Button></div> : null}
              {authSettingsLoading ? <p className="z8-auth-status" role="status">正在读取账户安全设置，请稍候。</p> : null}
              {captchaProvider.kind === "invalid" && !pendingTwoFactor ? <p className="z8-auth-status" role="alert">{captchaProvider.reason} 当前无法继续登录、注册或发送验证码。</p> : null}
              {agreement.kind === "unavailable" && !authSettingsLoading && !authSettingsError && !pendingTwoFactor ? <p className="z8-auth-status" role="alert">{agreement.reason}</p> : null}
              {!pendingTwoFactor ? <div className="z8-auth-fields">
                <div className="field"><Label htmlFor={`${fieldId}-email`}>邮箱</Label><Input id={`${fieldId}-email`} value={email} onChange={(event) => {
                  const nextEmail = event.currentTarget.value;
                  setEmail(nextEmail);
                  if (verificationEmail && nextEmail.trim().toLowerCase() !== verificationEmail) {
                    setVerifyCode("");
                    setVerificationDialogOpen(false);
                  }
                }} autoComplete="email" /></div>
                <div className="field"><Label htmlFor={`${fieldId}-password`}>密码</Label><div className="z8-password-field"><Input id={`${fieldId}-password`} type={showPassword ? "text" : "password"} value={password} onChange={(event) => setPassword(event.currentTarget.value)} autoComplete={registerMode ? "new-password" : "current-password"} /><Button type="button" variant="ghost" size="icon" className="z8-password-toggle" onClick={() => setShowPassword((value) => !value)} aria-label={showPassword ? "隐藏密码" : "显示密码"}>{showPassword ? <EyeOff aria-hidden="true" /> : <Eye aria-hidden="true" />}</Button></div></div>
                {registerMode ? <div className="field"><Label htmlFor={`${fieldId}-confirm-password`}>确认密码</Label><div className="z8-password-field"><Input id={`${fieldId}-confirm-password`} type={showConfirmPassword ? "text" : "password"} value={confirmPassword} onChange={(event) => setConfirmPassword(event.currentTarget.value)} autoComplete="new-password" aria-invalid={confirmPassword.length > 0 && password !== confirmPassword} /><Button type="button" variant="ghost" size="icon" className="z8-password-toggle" onClick={() => setShowConfirmPassword((value) => !value)} aria-label={showConfirmPassword ? "隐藏确认密码" : "显示确认密码"}>{showConfirmPassword ? <EyeOff aria-hidden="true" /> : <Eye aria-hidden="true" />}</Button></div>{confirmPassword.length > 0 && password !== confirmPassword ? <p className="field-hint bad">两次输入的密码不一致。</p> : null}</div> : null}
              </div> : null}
              {captchaWidgetProvider && !pendingTwoFactor && !verificationStepActive ? <div className="z8-auth-captcha"><CaptchaChallenge provider={captchaWidgetProvider} resetNonce={captchaResetNonce} onProof={handleCaptchaProof} /></div> : null}
              {registerMode && emailVerificationEnabled ? <div className="z8-registration-verification">
                <div><strong>邮箱验证</strong><span>{verificationSentForCurrentEmail ? "验证码已发送，请查收邮箱。" : "注册前需要验证邮箱。"}</span></div>
                <Button type="button" variant="secondary" disabled={busy !== null || !email || captchaConfigurationInvalid || !authSubmitReady} onClick={() => { if (verificationSentForCurrentEmail) setVerificationDialogOpen(true); else void sendVerificationCode(); }}>{verificationSentForCurrentEmail ? "输入验证码" : "发送验证码"}</Button>
              </div> : null}
              {SHOW_REGISTRATION_CODES && registerMode && authSettings?.invitationCodeEnabled ? <div className="field"><Label htmlFor={`${fieldId}-invite`}>邀请码</Label><Input id={`${fieldId}-invite`} value={invitationCode} onChange={(event) => setInvitationCode(event.currentTarget.value)} autoComplete="off" maxLength={256} required /></div> : null}
              {SHOW_REGISTRATION_CODES && registerMode && authSettings?.promoCodeEnabled ? <div className="field"><Label htmlFor={`${fieldId}-promo`}>优惠码（选填）</Label><Input id={`${fieldId}-promo`} value={promoCode} onChange={(event) => setPromoCode(event.currentTarget.value)} autoComplete="off" maxLength={256} /></div> : null}
              {pendingTwoFactor ? <div className="field"><Label htmlFor={`${fieldId}-two-factor`}>二次验证码{pendingEmail ? `（${pendingEmail}）` : ""}</Label><Input id={`${fieldId}-two-factor`} value={twoFactorCode} onChange={(event) => setTwoFactorCode(event.currentTarget.value)} inputMode="numeric" autoComplete="one-time-code" /></div> : null}
              {!pendingTwoFactor && agreement.kind === "required" ? (
                <section className="z8-agreement z8-auth-agreement" aria-label="Z8 账户协议">
                  {agreement.mode === "checkbox" ? (
                    <div className="z8-agreement-consent">
                      <input id={`${fieldId}-agreement`} type="checkbox" checked={acceptedAgreementIdentity === agreement.identity}
                        onChange={(event) => setAcceptedAgreementIdentity(event.currentTarget.checked ? agreement.identity : null)} />
                      <div className="z8-agreement-consent-copy">
                        <label htmlFor={`${fieldId}-agreement`}>我已阅读并同意</label>{" "}
                        {agreementDocumentList}
                      </div>
                    </div>
                  ) : (
                    <div className="z8-agreement-consent">
                      <Button type="button" variant="secondary" onClick={() => agreementDialogRef.current?.showModal()}>
                        {acceptedAgreementIdentity === agreement.identity ? "查看已同意的协议" : "查看并确认协议"}
                      </Button>
                      {acceptedAgreementIdentity === agreement.identity ? (
                        <Button type="button" variant="secondary" onClick={() => setAcceptedAgreementIdentity(null)}>撤回同意</Button>
                      ) : null}
                      <dialog ref={agreementDialogRef} className="z8-agreement-dialog" aria-label="确认 Z8 账户协议">
                        <h3>Z8 账户协议</h3>
                        <p>请打开并阅读以下协议。确认后才能继续{registerMode ? "注册" : "登录"}。</p>
                        {agreementDocumentList}
                        <div className="toolbar">
                          <Button type="button" variant="secondary" onClick={() => agreementDialogRef.current?.close()}>暂不同意</Button>
                          <Button type="button" onClick={() => { setAcceptedAgreementIdentity(agreement.identity); agreementDialogRef.current?.close(); }}>我已阅读并同意</Button>
                        </div>
                      </dialog>
                    </div>
                  )}
                </section>
              ) : null}
              <div className="z8-auth-actions">
                <Button type="submit" className="z8-auth-submit" disabled={!authSubmitReady}>{busy ? "处理中…" : pendingTwoFactor ? "提交验证" : registerMode ? (verificationStepActive || verificationSentForCurrentEmail ? "输入验证码" : "继续") : "登录"}</Button>
                {pendingTwoFactor ? <Button type="button" variant="secondary" className="z8-auth-cancel" disabled={accountLoading || busy !== null} onClick={() => void cancelLogin()}>取消登录</Button> : registrationEnabled ? <Button type="button" variant="ghost" className="z8-auth-switch" disabled={accountLoading || busy !== null || authSettingsLoading} onClick={() => { setRegisterMode((value) => !value); setShowPassword(false); setShowConfirmPassword(false); setConfirmPassword(""); setVerificationDialogOpen(false); setVerificationSent(false); setVerificationEmail(""); setVerificationCooldown(0); verificationAvailableAt.current = 0; setVerifyCode(""); }}>{registerMode ? "已有账户？登录" : "没有账户？注册"}</Button> : <span className="field-hint">当前暂未开放注册</span>}
              </div>
            </Z8SubmitForm>
          ) : (
            <div className="z8-account-shell">
              <header className="z8-account-toolbar">
                <div><p className="z8-account-eyebrow">Z8 ACCOUNT</p><h2>Z8 账户</h2></div>
                <div className="z8-account-toolbar-actions">
                  <Button variant="secondary" disabled={busy !== null} onClick={() => void refreshAccount("z8_refresh_session")} title="刷新账户会话"><RefreshCw aria-hidden="true" />刷新</Button>
                  <Button ref={redeemTriggerRef} variant="secondary" className="z8-recharge-trigger" disabled={busy !== null} onClick={() => redeemDialogRef.current?.showModal()} title="充值或兑换权益"><CreditCard aria-hidden="true" />充值</Button>
                </div>
              </header>
              <div className="z8-account-columns">
                <div className="z8-account-column z8-account-left">
                  <section className="z8-account-banner" aria-label="账户状态">
                    <div className="z8-account-avatar" aria-hidden="true"><UserRound /></div>
                    <div className="z8-account-identity"><strong>{account.email ?? "已登录"}</strong><span><span className="z8-account-status-dot" aria-hidden="true" />账户正常</span></div>
                    <Button variant="ghost" disabled={busy !== null} onClick={() => void logout()} title="退出 Z8 账户"><LogOut aria-hidden="true" />退出</Button>
                  </section>
                  <section className="z8-provider-section z8-key-section" aria-labelledby={`${fieldId}-provider-heading`}>
                    <div className="z8-section-heading"><div><h3 id={`${fieldId}-provider-heading`}>选择 API Key</h3><p>选择后立即写入 Z8 Provider</p></div><div className="z8-section-heading-actions"><Button variant="outline" disabled={busy !== null || !selectedKey || !usableKeys.some((key) => key.id === selectedKey)} onClick={() => void handleCheck()} title="检查 Z8 Provider"><Activity aria-hidden="true" />检查</Button><Button variant="outline" disabled={busy !== null || !onReset} onClick={() => void handleReset()} title="恢复 Z8 默认供应商配置"><RotateCcw aria-hidden="true" />重置配置</Button></div></div>
                    {account.keys.length ? <div className="z8-key-select-row"><KeyRound aria-hidden="true" /><Label htmlFor={`${fieldId}-key`} className="sr-only">API Key</Label><select id={`${fieldId}-key`} value={selectedKey} onChange={(event) => { const nextKey = event.currentTarget.value; resetLaunchState(); setSelectedKey(nextKey); setProviderCheck(null); setProviderAppliedKey(null); void handleApply(nextKey); }} disabled={busy !== null || launchState === "starting"}><option value="" disabled>请选择 API Key</option>{account.keys.map((key) => <option key={key.id} value={key.id} disabled={!isUsableApiKey(key)}>{key.name} · {key.secret.masked} · {key.status}</option>)}</select><Button variant="secondary" disabled={busy !== null || !selectedKey || !usableKeys.some((key) => key.id === selectedKey)} onClick={() => void handleApply()} title="应用当前 API Key"><Check aria-hidden="true" />使用</Button></div> : <p className="field-hint">当前账户没有可用 API Key，请先兑换或刷新。</p>}
                    {providerAppliedKey === selectedKey && selectedKey || providerCheck ? (
                      <div className="z8-provider-status-row" aria-live="polite">
                        {providerAppliedKey === selectedKey && selectedKey ? <p className="field-hint good z8-provider-applied" role="status">已写入 Z8 Provider，可启动 Codex。</p> : <span aria-hidden="true" />}
                        {providerCheck ? <p role="status" className={`z8-provider-health ${providerCheck.status === "ok" || providerCheck.status === "healthy" ? "good" : "bad"}`}>{providerCheck.message}{providerCheck.latencyMs ? ` · ${providerCheck.latencyMs}ms` : ""}</p> : <span aria-hidden="true" />}
                      </div>
                    ) : null}
                  </section>
                  <footer className="z8-account-primary-actions">
                    <Button disabled={launchButtonDisabled} onClick={() => void handleLaunch()} title={launchState === "failed" ? "重试启动 Z8 Codex" : "启动 Z8 Codex"} aria-busy={launchState === "starting"}>
                      {launchState === "starting" ? <RefreshCw style={{ animation: "spin 850ms linear infinite" }} aria-hidden="true" /> : launchState === "started" ? <Check aria-hidden="true" /> : <Rocket aria-hidden="true" />}
                      {launchButtonLabel}
                    </Button>
                  </footer>
                </div>
                <section className="z8-usage-section z8-account-column z8-account-right" aria-labelledby={`${fieldId}-usage-heading`}>
                  <div className="z8-section-heading"><div><h3 id={`${fieldId}-usage-heading`}>余额与用量</h3><p>统计来自当前 API Key</p></div><span className="z8-usage-updated">{usage ? `更新于 ${new Date(usage.observedAtMs).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" })}` : "等待同步"}</span></div>
                  {usageLoading ? <p className="field-hint" role="status">正在读取账户用量…</p> : usageError ? <p className="field-hint" role="alert">{usageError}</p> : usage ? (
                    <>
                      <div className="z8-usage-balance"><span>可用余额</span><strong>{usage.isUnlimited ? "不限" : formatUsageAmount(usage.remaining ?? usage.balance ?? usage.quota?.remaining, usage.unit ?? usage.quota?.unit)}</strong><small>{usage.accountStatus || (usage.isValid === false ? "账户不可用" : "已同步")}</small></div>
                      <div className="z8-usage-metrics" aria-label="账户用量统计">
                        <div><span>今日请求</span><strong>{formatUsageNumber(usage.usage?.today?.requests)}</strong></div>
                        <div><span>今日 Token</span><strong>{formatUsageNumber(usage.usage?.today?.totalTokens)}</strong></div>
                        <div><span>今日实际消耗</span><strong>{formatUsageAmount(usage.usage?.today?.actualCost ?? usage.usage?.today?.cost, usage.unit)}</strong></div>
                      </div>
                      <p className="z8-usage-summary">累计 {formatUsageNumber(usage.usage?.total?.requests)} 次请求 · {formatUsageNumber(usage.usage?.total?.totalTokens)} Token · 实际消耗 {formatUsageAmount(usage.usage?.total?.actualCost ?? usage.usage?.total?.cost, usage.unit)}</p>
                    </>
                  ) : <p className="field-hint">暂无用量数据，请先选择可用的 API Key。</p>}
                </section>
              </div>
            </div>
          )}
          {notice ? <div className="z8-account-notice" role={account.authenticated ? "status" : "alert"} aria-live="polite">{notice}</div> : null}
        </CardContent>
      </Card>
      </div>

      {!account.authenticated && !accountLoading && registerMode && emailVerificationEnabled ? (
        <dialog ref={verificationDialogRef} className="z8-verification-dialog" aria-labelledby={`${fieldId}-verification-heading`} onClose={() => setVerificationDialogOpen(false)}>
          <div className="z8-verification-dialog-shell">
            <header className="z8-verification-dialog-head">
              <div><span className="z8-auth-eyebrow">Z8 账户</span><h3 id={`${fieldId}-verification-heading`}>验证你的邮箱</h3><p>验证码已发送至 {verificationEmail || email.trim()}</p></div>
              <Button type="button" variant="outline" size="icon" className="z8-verification-close" onClick={() => setVerificationDialogOpen(false)} aria-label="关闭邮箱验证窗口"><X aria-hidden="true" /></Button>
            </header>
            <div className="z8-verification-dialog-content">
              <Z8SubmitForm className="z8-verification-form" ready={verificationSubmitReady} onAction={() => void handleAuth()}>
                <div className="field"><Label htmlFor={`${fieldId}-verification-code`}>邮箱验证码</Label><Input id={`${fieldId}-verification-code`} value={verifyCode} onChange={(event) => setVerifyCode(event.currentTarget.value)} inputMode="numeric" autoComplete="one-time-code" maxLength={6} autoFocus /></div>
                <p className="z8-verification-hint">请输入邮件中的 6 位验证码。</p>
                {verificationStepActive && captchaWidgetProvider ? <div className="z8-auth-captcha z8-verification-captcha"><CaptchaChallenge provider={captchaWidgetProvider} resetNonce={captchaResetNonce} onProof={handleCaptchaProof} /></div> : null}
                <div className="z8-verification-actions">
                  <Button type="button" variant="secondary" disabled={busy !== null} onClick={() => setVerificationDialogOpen(false)}>返回修改</Button>
                  <Button type="submit" className="z8-auth-submit" disabled={!verificationSubmitReady}>{busy === "z8_register" ? "创建中…" : "验证并创建账户"}</Button>
                </div>
                <Button type="button" variant="ghost" className="z8-verification-resend" disabled={busy !== null || verificationCooldown > 0} onClick={() => void sendVerificationCode()}>{verificationCooldown > 0 ? `${verificationCooldown}秒后可重新发送` : "重新发送验证码"}</Button>
              </Z8SubmitForm>
            </div>
          </div>
        </dialog>
      ) : null}

      {account.authenticated ? (
        <dialog ref={redeemDialogRef} className="z8-redeem-dialog" aria-labelledby={`${fieldId}-redeem-heading`} onClose={() => redeemTriggerRef.current?.focus()}>
          <div className="z8-redeem-dialog-shell">
            <header className="z8-redeem-dialog-head">
              <div className="z8-redeem-title-group"><span className="z8-auth-eyebrow">Z8 账户</span><h3 id={`${fieldId}-redeem-heading`}>兑换权益</h3></div>
              <Button type="button" variant="outline" size="icon" className="z8-redeem-close" onClick={() => redeemDialogRef.current?.close()} aria-label="关闭兑换窗口"><X aria-hidden="true" /></Button>
            </header>
            <div className="z8-redeem-dialog-content">
              <Z8SubmitForm className="z8-redeem-form" ready={redeemSubmitReady} onAction={() => void handleRedeem()}>
                <div className="field"><Label htmlFor={`${fieldId}-redeem`}>兑换码</Label><Input id={`${fieldId}-redeem`} value={redeemCode} onChange={(event) => setRedeemCode(event.currentTarget.value)} autoFocus /></div>
                <div className="z8-redeem-actions">
                  <div className="z8-redeem-form-actions"><Button type="button" variant="secondary" disabled={busy !== null} onClick={() => redeemDialogRef.current?.close()}>取消</Button><Button type="submit" className="z8-redeem-submit" disabled={!redeemSubmitReady}>{busy === "z8_redeem" ? "兑换中…" : "兑换"}</Button></div>
                  <Button type="button" variant="outline" className="z8-purchase-button" onClick={() => void openPurchasePage()} title="购买兑换额度"><ExternalLink aria-hidden="true" />购买兑换额度</Button>
                </div>
              </Z8SubmitForm>
            </div>
          </div>
        </dialog>
      ) : null}
    </div>
  );
}
