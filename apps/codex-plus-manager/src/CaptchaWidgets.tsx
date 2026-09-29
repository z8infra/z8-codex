import { useCallback, useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import type { CaptchaProvider } from "./account-flow";

export type CaptchaProof = {
  turnstileToken?: string;
  tencentCaptchaTicket?: string;
  tencentCaptchaRandstr?: string;
};

type CaptchaWidgetProps = {
  provider: Exclude<CaptchaProvider, { kind: "none" } | { kind: "invalid" }>;
  resetNonce: number;
  onProof: (proof: CaptchaProof | null) => void;
};

type WidgetState = "loading" | "ready" | "success" | "failed" | "cancelled";

type TurnstileApi = {
  render: (container: HTMLElement, options: Record<string, unknown>) => string;
  remove: (widgetId: string) => void;
};

type TencentResult = { ret?: number; ticket?: string | null; randstr?: string | null };
type TencentInstance = { show: () => void; destroy: () => void };
type TencentConstructor = {
  new (appId: string, callback: (result: TencentResult) => void, options?: Record<string, unknown>): TencentInstance;
  new (container: HTMLElement, appId: string, callback: (result: TencentResult) => void, options?: Record<string, unknown>): TencentInstance;
};

type AliyunOptions = {
  SceneId: string;
  prefix: string;
  mode: "popup";
  element: string;
  button: string;
  captchaVerifyCallback: (param: unknown) => { captchaResult: boolean };
  onBizResultCallback: (result: boolean) => void;
  getInstance: (instance: unknown) => void;
};

declare global {
  interface Window {
    turnstile?: TurnstileApi;
    TencentCaptcha?: TencentConstructor;
    TCaptchaGlobal?: boolean;
    initAliyunCaptcha?: (options: AliyunOptions) => void;
    AliyunCaptchaConfig?: { region: string; prefix: string };
  }
}

const SCRIPT_TIMEOUT_MS = 30_000;
const TOKEN_LIMIT = 4096;

function bounded(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const result = value.trim();
  return result.length > 0 && result.length <= TOKEN_LIMIT ? result : null;
}

function loadScript(id: string, src: string, ready: () => boolean): Promise<void> {
  if (ready()) return Promise.resolve();
  const existing = document.querySelector<HTMLScriptElement>(`script[data-z8-captcha="${id}"]`);
  if (existing) {
    return new Promise((resolve, reject) => {
      const timer = window.setTimeout(() => reject(new Error("captcha_timeout")), SCRIPT_TIMEOUT_MS);
      existing.addEventListener("load", () => { window.clearTimeout(timer); ready() ? resolve() : reject(new Error("captcha_unavailable")); }, { once: true });
      existing.addEventListener("error", () => { window.clearTimeout(timer); reject(new Error("captcha_unavailable")); }, { once: true });
    });
  }
  return new Promise((resolve, reject) => {
    const script = document.createElement("script");
    script.dataset.z8Captcha = id;
    script.src = src;
    script.async = true;
    let settled = false;
    const finish = (error?: Error) => {
      if (settled) return;
      settled = true;
      window.clearTimeout(timer);
      script.removeEventListener("load", loaded);
      script.removeEventListener("error", failed);
      if (error) { script.remove(); reject(error); } else resolve();
    };
    const loaded = () => ready() ? finish() : finish(new Error("captcha_unavailable"));
    const failed = () => finish(new Error("captcha_unavailable"));
    const timer = window.setTimeout(() => finish(new Error("captcha_timeout")), SCRIPT_TIMEOUT_MS);
    script.addEventListener("load", loaded);
    script.addEventListener("error", failed);
    document.head.appendChild(script);
  });
}

function useWidgetLifecycle(resetNonce: number, onProof: (proof: CaptchaProof | null) => void) {
  const onProofRef = useRef(onProof);
  const [state, setState] = useState<WidgetState>("loading");
  const [retryNonce, setRetryNonce] = useState(0);
  const cancelRef = useRef<(() => void) | null>(null);
  useEffect(() => { onProofRef.current = onProof; }, [onProof]);
  useEffect(() => {
    setState("loading");
    cancelRef.current = null;
    onProofRef.current(null);
    return () => { cancelRef.current?.(); cancelRef.current = null; };
  }, [resetNonce, retryNonce]);
  const cancel = useCallback(() => {
    cancelRef.current?.();
    cancelRef.current = null;
    onProofRef.current(null);
    setState("cancelled");
  }, []);
  const retry = useCallback(() => setRetryNonce((value) => value + 1), []);
  return { state, setState, cancelRef, cancel, retry };
}

function statusText(state: WidgetState): string {
  if (state === "loading") return "正在准备安全验证…";
  if (state === "ready") return "请完成安全验证。";
  if (state === "success") return "安全验证已完成。";
  if (state === "cancelled") return "安全验证已取消，请重试。";
  return "安全验证加载失败，请检查网络后重试。";
}

function WidgetFrame({ state, onCancel, onRetry, children }: { state: WidgetState; onCancel: () => void; onRetry: () => void; children?: ReactNode }) {
  return <div className={`account-captcha ${state}`} aria-live="polite">
    {children}
    <p className="field-hint" role={state === "failed" ? "alert" : "status"}>{statusText(state)}</p>
    {state === "ready" || state === "loading" ? <button type="button" className="account-captcha-button" onClick={onCancel}>取消验证</button> : null}
    {state === "failed" || state === "cancelled" ? <button type="button" className="account-captcha-button" onClick={onRetry}>重试验证</button> : null}
  </div>;
}

function TurnstileWidget({ provider, resetNonce, onProof }: CaptchaWidgetProps & { provider: Extract<CaptchaProvider, { kind: "turnstile" }> }) {
  const containerRef = useRef<HTMLDivElement>(null);
  const widgetRef = useRef<string | null>(null);
  const apiRef = useRef<TurnstileApi | null>(null);
  const lifecycle = useWidgetLifecycle(resetNonce, onProof);
  useEffect(() => {
    let active = true;
    const controller = new AbortController();
    const clear = () => {
      if (widgetRef.current && apiRef.current) {
        try { apiRef.current.remove(widgetRef.current); } catch { /* Third-party widget may already be gone. */ }
      }
      widgetRef.current = null;
    };
    lifecycle.cancelRef.current = () => { controller.abort(); clear(); };
    void loadScript("turnstile", "https://challenges.cloudflare.com/turnstile/v0/api.js?render=explicit", () => Boolean(window.turnstile))
      .then(() => {
        if (!active || controller.signal.aborted || !containerRef.current || !window.turnstile) return;
        apiRef.current = window.turnstile;
        lifecycle.setState("ready");
        widgetRef.current = window.turnstile.render(containerRef.current, {
          sitekey: provider.siteKey,
          action: "z8_account",
          appearance: "always",
          language: "zh-CN",
          callback: (token: unknown) => {
            const boundedToken = bounded(token);
            if (!active || !boundedToken) { lifecycle.setState("failed"); lifecycle.cancelRef.current?.(); return; }
            lifecycle.setState("success");
            onProof({ turnstileToken: boundedToken });
          },
          "expired-callback": () => { if (active) { lifecycle.setState("failed"); onProof(null); } },
          "error-callback": () => { if (active) { lifecycle.setState("failed"); onProof(null); } },
        });
      })
      .catch(() => { if (active) { lifecycle.setState("failed"); onProof(null); } });
    return () => { active = false; controller.abort(); clear(); onProof(null); };
  }, [provider.siteKey, resetNonce, lifecycle.retry, lifecycle.setState, lifecycle.cancelRef, onProof]);
  return <WidgetFrame state={lifecycle.state} onCancel={lifecycle.cancel} onRetry={lifecycle.retry}><div ref={containerRef} /></WidgetFrame>;
}

function TencentWidget({ provider, resetNonce, onProof }: CaptchaWidgetProps & { provider: Extract<CaptchaProvider, { kind: "tencent" }> }) {
  const containerRef = useRef<HTMLDivElement>(null);
  const instanceRef = useRef<TencentInstance | null>(null);
  const lifecycle = useWidgetLifecycle(resetNonce, onProof);
  const region = provider.region === "intl" ? "intl" : "cn";
  useEffect(() => {
    let active = true;
    const script = region === "intl" ? "https://ca.turing.captcha.qcloud.com/TJNCaptcha-global.js" : "https://turing.captcha.qcloud.com/TJCaptcha.js";
    lifecycle.cancelRef.current = () => { instanceRef.current?.destroy(); instanceRef.current = null; };
    void loadScript(`tencent-${region}`, script, () => Boolean(window.TencentCaptcha && (region === "intl") === (window.TCaptchaGlobal === true)))
      .then(() => {
        if (!active || !containerRef.current || !window.TencentCaptcha) return;
        const callback = (result: TencentResult) => {
          if (!active) return;
          const ticket = bounded(result.ticket);
          const randstr = bounded(result.randstr);
          if (result.ret === 0 && ticket && randstr && !ticket.startsWith("trerror_")) { lifecycle.setState("success"); onProof({ tencentCaptchaTicket: ticket, tencentCaptchaRandstr: randstr }); }
          else { lifecycle.setState("failed"); onProof(null); }
        };
        instanceRef.current = region === "intl"
          ? new window.TencentCaptcha(containerRef.current, provider.appId, callback, { enableAutoCheck: false, userLanguage: "zh-cn", type: "popup" })
          : new window.TencentCaptcha(provider.appId, callback, { userLanguage: "zh-cn" });
        lifecycle.setState("ready");
      })
      .catch(() => { if (active) { lifecycle.setState("failed"); onProof(null); } });
    return () => { active = false; instanceRef.current?.destroy(); instanceRef.current = null; onProof(null); };
  }, [provider.appId, region, resetNonce, lifecycle.retry, lifecycle.setState, lifecycle.cancelRef, onProof]);
  return <WidgetFrame state={lifecycle.state} onCancel={lifecycle.cancel} onRetry={lifecycle.retry}><div ref={containerRef} /><button type="button" className="account-captcha-button" onClick={() => instanceRef.current?.show()} disabled={lifecycle.state !== "ready"}>点击完成安全验证</button></WidgetFrame>;
}

function AliyunWidget({ provider, resetNonce, onProof }: CaptchaWidgetProps & { provider: Extract<CaptchaProvider, { kind: "aliyun" }> }) {
  const id = useRef(Math.random().toString(36).slice(2)).current;
  const buttonId = `z8-aliyun-button-${id}`;
  const elementId = `z8-aliyun-element-${id}`;
  const lifecycle = useWidgetLifecycle(resetNonce, onProof);
  useEffect(() => {
    let active = true;
    lifecycle.cancelRef.current = () => { document.getElementById("aliyunCaptcha-mask")?.remove(); document.getElementById("aliyunCaptcha-window-popup")?.remove(); };
    window.AliyunCaptchaConfig = { region: provider.region === "sgp" ? "sgp" : "cn", prefix: provider.prefix };
    void loadScript("aliyun", "https://o.alicdn.com/captcha-frontend/aliyunCaptcha/AliyunCaptcha.js", () => Boolean(window.initAliyunCaptcha))
      .then(() => {
        if (!active || !window.initAliyunCaptcha) return;
        window.initAliyunCaptcha({
          SceneId: provider.sceneId,
          prefix: provider.prefix,
          mode: "popup",
          element: `#${elementId}`,
          button: `#${buttonId}`,
          captchaVerifyCallback: (param: unknown) => {
            const proof = bounded(param);
            if (active && proof) { lifecycle.setState("success"); onProof({ turnstileToken: proof }); }
            else if (active) { lifecycle.setState("failed"); onProof(null); }
            return { captchaResult: Boolean(proof) };
          },
          onBizResultCallback: () => {},
          getInstance: () => {},
        });
        lifecycle.setState("ready");
      })
      .catch(() => { if (active) { lifecycle.setState("failed"); onProof(null); } });
    return () => { active = false; lifecycle.cancelRef.current?.(); onProof(null); };
  }, [provider.prefix, provider.region, provider.sceneId, resetNonce, lifecycle.retry, lifecycle.setState, lifecycle.cancelRef, onProof, buttonId, elementId]);
  return <WidgetFrame state={lifecycle.state} onCancel={lifecycle.cancel} onRetry={lifecycle.retry}><div id={elementId} /><button id={buttonId} type="button" className="account-captcha-button" disabled={lifecycle.state !== "ready"}>点击完成安全验证</button></WidgetFrame>;
}

export function CaptchaChallenge({ provider, resetNonce, onProof }: CaptchaWidgetProps) {
  if (provider.kind === "turnstile") return <TurnstileWidget provider={provider} resetNonce={resetNonce} onProof={onProof} />;
  if (provider.kind === "tencent") return <TencentWidget provider={provider} resetNonce={resetNonce} onProof={onProof} />;
  return <AliyunWidget provider={provider} resetNonce={resetNonce} onProof={onProof} />;
}
