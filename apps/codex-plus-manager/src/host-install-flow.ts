export type HostStatus = {
  status: string;
  message: string;
  installed: boolean;
  supported: boolean;
  platform: string;
  version?: string | null;
  selectedAsset?: string | null;
  resumable?: boolean;
  reason?: string | null;
};

export type HostInstallProgress = {
  phase: "download" | "verify" | "install" | "complete";
  completedBytes: number;
  totalBytes: number;
};

type HostInstallResult = {
  status: string;
  message: string;
  installed: boolean;
  resumable?: boolean;
};

export type HostInstallPhase = "idle" | "checking" | "download" | "verify" | "install" | "paused" | "complete" | "failed";

export type HostInstallState = {
  open: boolean;
  phase: HostInstallPhase;
  host: HostStatus | null;
  progress: HostInstallProgress | null;
  error: string | null;
  resumable: boolean;
  speedBytesPerSecond: number;
  cancelRequested: boolean;
};

type HostInstallDependencies = {
  invoke: <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
  listenProgress: (handler: (progress: HostInstallProgress) => void) => Promise<() => void>;
};

const initialState: HostInstallState = {
  open: false,
  phase: "idle",
  host: null,
  progress: null,
  error: null,
  resumable: false,
  speedBytesPerSecond: 0,
  cancelRequested: false,
};

function isSuccessfulStatus(status: string): boolean {
  return status === "ok" || status === "accepted";
}

function isBusy(phase: HostInstallPhase): boolean {
  return phase === "download" || phase === "verify" || phase === "install";
}

/** Owns the explicit-consent boundary between host detection and installation. */
export class HostInstallFlow {
  private state: HostInstallState = initialState;
  private listener: ((state: HostInstallState) => void) | null = null;
  private revision = 0;
  private lastProgressAt = 0;
  private lastProgressBytes = 0;
  private progressInitialized = false;

  constructor(private readonly dependencies: HostInstallDependencies) {}

  getState(): HostInstallState {
    return this.state;
  }

  subscribe(listener: (state: HostInstallState) => void): () => void {
    this.listener = listener;
    listener(this.state);
    return () => {
      if (this.listener === listener) this.listener = null;
    };
  }

  private update(patch: Partial<HostInstallState>): void {
    this.state = { ...this.state, ...patch };
    this.listener?.(this.state);
  }

  close(): void {
    if (isBusy(this.state.phase)) return;
    // A late check must not reopen a dialog the user just dismissed or start
    // the waiting launch action after dismissal.
    // The first startup check runs while the dialog is still closed, so its
    // phase remains idle until the response arrives. Invalidate every pending
    // check here, not only checks that have already rendered as checking.
    this.revision += 1;
    this.update({ open: false, phase: "idle", speedBytesPerSecond: 0, cancelRequested: false });
  }

  async check(openWhenMissing: boolean, appPath?: string): Promise<boolean> {
    if (isBusy(this.state.phase)) return false;
    const revision = ++this.revision;
    if (this.state.open) this.update({ phase: "checking", error: null });
    try {
      const args = appPath === undefined ? undefined : { appPath };
      const host = await this.dependencies.invoke<HostStatus>("z8_host_status", args);
      if (revision !== this.revision) return false;
      if (!isSuccessfulStatus(host.status) || typeof host.installed !== "boolean" || typeof host.supported !== "boolean") {
        throw new Error("host-status-not-ready");
      }
      if (host.installed) {
        this.update({
          host,
          phase: this.state.open ? "complete" : "idle",
          error: null,
          progress: null,
          resumable: false,
          speedBytesPerSecond: 0,
          cancelRequested: false,
        });
        return true;
      }
      this.update({
        host,
        open: openWhenMissing || this.state.open,
        phase: "idle",
        error: null,
        progress: null,
        resumable: Boolean(host.resumable),
        speedBytesPerSecond: 0,
        cancelRequested: false,
      });
      return false;
    } catch {
      if (revision === this.revision) {
        this.update({
          open: openWhenMissing || this.state.open,
          phase: "failed",
          host: null,
          progress: null,
          error: "暂时无法确认本机是否已安装 Codex 桌面版。请重试检测。",
          speedBytesPerSecond: 0,
          cancelRequested: false,
        });
      }
      return false;
    }
  }

  async requireHost(appPath: string): Promise<boolean> {
    const installed = await this.check(true, appPath);
    if (installed) this.close();
    return installed;
  }

  async install(): Promise<boolean> {
    if (!this.state.open || !this.state.host?.supported || !this.state.host.selectedAsset || this.state.host.installed
      || this.state.phase === "checking" || isBusy(this.state.phase)) {
      return false;
    }
    this.lastProgressAt = Date.now();
    this.lastProgressBytes = 0;
    this.progressInitialized = false;
    this.update({ phase: "download", error: null, progress: null, speedBytesPerSecond: 0, cancelRequested: false });
    let unlisten: (() => void) | undefined;
    try {
      // Register before invoking so even the first download event is visible.
      unlisten = await this.dependencies.listenProgress((progress) => {
        if (!isBusy(this.state.phase)) return;
        const phase = progress.phase === "complete" ? "install" : progress.phase;
        const now = Date.now();
        let speedBytesPerSecond = this.state.speedBytesPerSecond;
        if (this.progressInitialized) {
          const elapsed = now - this.lastProgressAt;
          const bytesDelta = progress.completedBytes - this.lastProgressBytes;
          if (elapsed > 0 && bytesDelta > 0) {
            const instant = Math.round((bytesDelta * 1000) / elapsed);
            speedBytesPerSecond = speedBytesPerSecond > 0
              ? Math.round(speedBytesPerSecond * 0.35 + instant * 0.65)
              : instant;
          }
        } else {
          this.progressInitialized = true;
        }
        this.lastProgressAt = now;
        this.lastProgressBytes = progress.completedBytes;
        this.update({ phase, progress, speedBytesPerSecond });
      });
      const result = await this.dependencies.invoke<HostInstallResult>("z8_install_host", { confirmed: true });
      if (!isSuccessfulStatus(result.status) || !result.installed) {
        const resumable = Boolean(result.resumable);
        this.update({
          phase: resumable ? "paused" : "failed",
          resumable,
          cancelRequested: false,
          error: result.message || "安装未完成，请重试。",
        });
        return false;
      }
    } catch {
      // A selected asset only proves that the platform is supported; it does
      // not prove that a partial file exists. Keep the continue action hidden
      // when the request failed before any bytes were written.
      const resumable = this.state.resumable || Boolean(this.state.progress?.completedBytes);
      this.update({
        phase: resumable ? "paused" : "failed",
        resumable,
        cancelRequested: false,
        error: "下载或安装未完成。请检查网络连接和磁盘空间后重试。",
      });
      return false;
    } finally {
      unlisten?.();
    }
    this.update({ phase: "checking" });
    const installed = await this.check(true);
    if (!installed && this.state.host?.installed === false) {
      this.update({ phase: "failed", error: "安装流程已结束，但仍未检测到 Codex 桌面版。请重试检测。" });
    }
    return installed;
  }

  async cancel(): Promise<boolean> {
    if (isBusy(this.state.phase)) {
      if (this.state.cancelRequested) return true;
      try {
        const result = await this.dependencies.invoke<HostInstallResult>("z8_cancel_host_install");
        if (!isSuccessfulStatus(result.status)) {
          this.update({ error: result.message || "无法取消安装。" });
          return false;
        }
        this.update({ cancelRequested: true, error: result.message || null });
        return true;
      } catch {
        this.update({ error: "无法取消安装，请稍后重试。" });
        return false;
      }
    }
    this.close();
    return true;
  }
}
