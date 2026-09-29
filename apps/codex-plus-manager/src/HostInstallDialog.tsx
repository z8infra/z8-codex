import { Button } from "@/components/ui/button";
import { Download, LogIn, Play, RefreshCw, X } from "lucide-react";
import { t } from "@/i18n";
import type { HostInstallPhase, HostInstallState } from "./host-install-flow";

export function hostPlatformLabel(platform: string): string {
  const labels: Record<string, string> = {
    "windows-x64": "Windows x64（Intel / AMD）",
    "windows-arm64": "Windows ARM64",
    "macos-x64": "macOS Intel",
    "macos-arm64": "macOS Apple Silicon（M 系列）",
    windows: "Windows",
    macos: "macOS",
    unsupported: "其他系统",
  };
  return labels[platform] ?? platform;
}

const phaseLabels: Record<HostInstallPhase, string> = {
  idle: "等待操作",
  checking: "正在检测本机 Codex…",
  download: "正在下载 Codex 桌面版…",
  verify: "正在校验安装包…",
  install: "正在安装 Codex 桌面版…",
  paused: "安装已暂停，可继续安装",
  complete: "Codex 桌面版已就绪",
  failed: "操作未完成",
};

export function formatHostDownloadBytes(value: number): string {
  if (!Number.isFinite(value) || value < 0) return "—";
  if (value < 1024) return `${Math.floor(value)} B`;
  if (value < 1024 ** 2) return `${(value / 1024).toFixed(1)} KB`;
  if (value < 1024 ** 3) return `${(value / 1024 ** 2).toFixed(1)} MB`;
  return `${(value / 1024 ** 3).toFixed(2)} GB`;
}

export function HostInstallDialog({
  state,
  onClose,
  onInstall,
  onContinue = onInstall,
  onCancel = onClose,
  onRecheck,
  onAccount,
}: {
  state: HostInstallState;
  onClose: () => void;
  onInstall: () => void;
  onContinue?: () => void;
  onCancel?: () => void;
  onRecheck: () => void;
  onAccount: () => void;
}) {
  if (!state.open) return null;
  const busy = ["download", "verify", "install"].includes(state.phase);
  const hasTotal = Boolean(state.progress && Number.isFinite(state.progress.totalBytes) && state.progress.totalBytes > 0);
  const completed = state.progress && Number.isFinite(state.progress.completedBytes)
    ? Math.max(0, state.progress.completedBytes)
    : 0;
  const total = hasTotal ? state.progress!.totalBytes : 0;
  const percent = total > 0 ? Math.min(100, Math.round((completed / total) * 100)) : 0;
  const canInstall = state.host?.installed === false && state.host.supported && Boolean(state.host.selectedAsset) && state.phase !== "checking" && !busy;
  const focusDialog = (dialog: HTMLDivElement | null) => {
    if (!dialog || typeof document === "undefined" || dialog.contains(document.activeElement)) return;
    const action = dialog.querySelector<HTMLButtonElement>(".z8-host-install-actions button:not(:disabled)");
    const dismiss = dialog.querySelector<HTMLButtonElement>(".toast-close:not(:disabled)");
    (action ?? dismiss ?? dialog).focus();
  };

  return (
    <div className="modal-backdrop" role="presentation">
      <div
        aria-describedby="z8-host-install-description"
        aria-labelledby="z8-host-install-title"
        aria-modal="true"
        className="modal-card z8-host-install-dialog"
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            if (!busy) onClose();
            return;
          }
          if (event.key !== "Tab") return;
          const buttons = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>("button:not(:disabled)"));
          if (buttons.length === 0) {
            event.preventDefault();
            event.currentTarget.focus();
            return;
          }
          const first = buttons[0];
          const last = buttons[buttons.length - 1];
          const active = document.activeElement;
          if (active === event.currentTarget || !event.currentTarget.contains(active)) {
            event.preventDefault();
            (event.shiftKey ? last : first).focus();
          } else if (event.shiftKey && active === first) {
            event.preventDefault();
            last.focus();
          } else if (!event.shiftKey && active === last) {
            event.preventDefault();
            first.focus();
          }
        }}
        ref={focusDialog}
        role="dialog"
        tabIndex={-1}
      >
        <div className="modal-head">
          <div>
            <span className="z8-host-install-eyebrow">Z8 CODEX · 环境检查</span>
            <h2 id="z8-host-install-title">{state.phase === "complete" ? t("Codex 桌面版已就绪") : t("安装 Codex 桌面版")}</h2>
          </div>
          {!busy ? <button aria-label={t("稍后处理")} className="toast-close" onClick={onClose} type="button">×</button> : null}
        </div>
        <div className="z8-host-install-body">
          {state.phase === "complete" ? (
            <p id="z8-host-install-description">{t("Codex 桌面版已安装。请先登录或创建 Z8 账户，系统会自动获取 API Key 并配置 Z8 Provider。")}</p>
          ) : (
            <p id="z8-host-install-description">{t("启动 Z8 Codex 前，本机需要安装官方 Codex 桌面版。已安装的用户可跳过此步骤；尚未安装请点击“安装”。")}</p>
          )}
          <div className="z8-host-install-facts">
            <div><span>{t("安装来源")}</span><strong>{t("Z8 提供的官方 Codex Desktop 镜像")}</strong></div>
            <div><span>{t("检测到的平台")}</span><strong>{state.host ? hostPlatformLabel(state.host.platform) : t("正在检测…")}</strong></div>
            <div><span>{t("匹配资源")}</span><strong>{state.host ? state.host.selectedAsset ?? t("暂无可用资源") : t("正在检测…")}</strong></div>
            <div><span>{t("镜像状态")}</span><strong>{t("点击下载后检查该平台资源是否已发布")}</strong></div>
            <div><span>{t("文件大小")}</span><strong>{hasTotal ? formatHostDownloadBytes(total) : t("文件较大，请预留磁盘空间；下载时显示实际大小")}</strong></div>
          </div>
          {state.host && !state.host.supported && !state.host.installed ? (
            <p className="z8-host-install-manual">
              {state.host.reason || t("当前系统不满足 Codex 桌面版镜像自动安装条件。请从 Codex 官方渠道手动安装桌面版，再返回这里点击“重新检测”。")}
            </p>
          ) : null}
          {state.host?.installed === false && state.host.supported && !state.host.selectedAsset ? (
            <p className="z8-host-install-manual">
              {t("暂时无法确认适合本机的镜像资源。请重新检测或更新 Z8 Codex。")}
            </p>
          ) : null}
          {state.host?.installed === false ? (
            <p className="z8-host-install-manual">
              {t("如果下载速度较慢，建议前往官方渠道或其他正规渠道下载并安装最新版 Codex 桌面版，完成后返回此处重新检测。")}
            </p>
          ) : null}
          {busy || state.phase === "checking" ? (
            <div aria-live="polite" className="z8-host-install-progress">
              <div className="z8-host-install-progress-head">
                <strong>{t(phaseLabels[state.phase])}</strong>
                <span>{total > 0 ? `${percent}%` : ""}{state.speedBytesPerSecond > 0 ? ` · ${formatHostDownloadBytes(state.speedBytesPerSecond)}/s` : ""}</span>
              </div>
              <progress aria-label={t("下载进度")} max={total > 0 ? total : undefined} value={total > 0 ? Math.min(completed, total) : undefined} />
              {state.progress && total > 0 ? <small>{formatHostDownloadBytes(completed)} / {formatHostDownloadBytes(total)}{state.speedBytesPerSecond > 0 ? ` · ${formatHostDownloadBytes(state.speedBytesPerSecond)}/s` : ""}</small> : null}
            </div>
          ) : null}
          {state.error ? <p aria-live="assertive" className="z8-host-install-error" role="alert">{state.error}</p> : null}
          {busy ? <p className="z8-host-install-hint">{t("安装正在进行，请保持此窗口打开。完成后会再次检查本机环境。")}</p> : null}
          {state.phase === "paused" && state.resumable ? <p className="z8-host-install-hint">{t(phaseLabels.paused)}</p> : null}
        </div>
        <div className="z8-host-install-actions">
          {busy ? (
            <>
              <Button disabled={state.cancelRequested} onClick={onCancel} type="button" variant="outline"><X className="h-4 w-4" />{state.cancelRequested ? t("正在取消安装…") : t("取消安装")}</Button>
              <Button disabled title={t("当前安装正在进行中")} type="button" variant="secondary"><Play className="h-4 w-4" />{t("继续安装")}</Button>
            </>
          ) : state.resumable && canInstall ? (
            <>
              <Button onClick={onContinue} type="button"><Play className="h-4 w-4" />{t("继续安装")}</Button>
              <Button onClick={onCancel} type="button" variant="secondary"><X className="h-4 w-4" />{t("取消安装")}</Button>
            </>
          ) : state.phase === "complete" ? (
            <Button onClick={onAccount} type="button"><LogIn className="h-4 w-4" />{t("登录 / 创建账户")}</Button>
          ) : canInstall ? (
            <Button onClick={onInstall} type="button"><Download className="h-4 w-4" />{state.resumable ? t("继续安装") : t("下载并安装")}</Button>
          ) : null}
          {!busy && !state.resumable && state.phase !== "complete" ? (
            <Button onClick={onRecheck} type="button" variant="outline"><RefreshCw className="h-4 w-4" />{t("重新检测")}</Button>
          ) : null}
          {!busy && !state.resumable ? <Button onClick={onClose} type="button" variant="secondary">{t("稍后处理")}</Button> : null}
        </div>
      </div>
    </div>
  );
}
