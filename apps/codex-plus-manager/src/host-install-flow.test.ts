import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";
import type { ReactElement, ReactNode } from "react";
import { HostInstallDialog, formatHostDownloadBytes, hostPlatformLabel } from "./HostInstallDialog";
import { HostInstallFlow, type HostInstallProgress, type HostStatus } from "./host-install-flow";

const missingHost: HostStatus = {
  status: "ok",
  message: "未安装",
  installed: false,
  supported: true,
  platform: "windows-x64",
  selectedAsset: "Codex-windows-x64.msix",
};

function findButton(node: ReactNode, text: string): ReactElement<{ onClick: () => void }> | null {
  if (!node || typeof node !== "object") return null;
  if (Array.isArray(node)) {
    for (const child of node) {
      const found = findButton(child, text);
      if (found) return found;
    }
    return null;
  }
  if (!("props" in node)) return null;
  const element = node as ReactElement<{ children?: ReactNode; onClick?: () => void }>;
  if (element.props.onClick && renderToStaticMarkup(element).includes(text)) {
    // A parent div may contain several buttons; only elements with onClick qualify.
    return element as ReactElement<{ onClick: () => void }>;
  }
  return findButton(element.props.children, text);
}

test("installed host keeps launch path free of download and preserves explicit app path", async () => {
  const commands: Array<[string, Record<string, unknown> | undefined]> = [];
  const flow = new HostInstallFlow({
    invoke: async <T,>(command: string, args?: Record<string, unknown>) => {
      commands.push([command, args]);
      return { ...missingHost, installed: true } as T;
    },
    listenProgress: async () => () => {},
  });
  assert.equal(await flow.check(true), true);
  assert.equal(flow.getState().open, false);
  assert.equal(await flow.requireHost("C:\\Portable\\Codex.exe"), true);
  assert.deepEqual(commands, [
    ["z8_host_status", undefined],
    ["z8_host_status", { appPath: "C:\\Portable\\Codex.exe" }],
  ]);
  assert.equal(renderToStaticMarkup(HostInstallDialog({
    state: flow.getState(), onClose: () => {}, onInstall: () => {}, onRecheck: () => {}, onAccount: () => {},
  })), "");
});

test("missing host dialog waits for its explicit download button before invoking install", async () => {
  const commands: Array<[string, Record<string, unknown> | undefined]> = [];
  let progressHandler: ((progress: HostInstallProgress) => void) | null = null;
  let unsubscribed = false;
  const flow = new HostInstallFlow({
    invoke: async <T,>(command: string, args?: Record<string, unknown>) => {
      commands.push([command, args]);
      if (command === "z8_host_status") {
        return (commands.length === 1 ? missingHost : { ...missingHost, installed: true }) as T;
      }
      assert.deepEqual(args, { confirmed: true });
      progressHandler?.({ phase: "download", completedBytes: 50, totalBytes: 100 });
      assert.equal(flow.getState().phase, "download");
      assert.match(renderToStaticMarkup(HostInstallDialog({
        state: flow.getState(), onClose: () => {}, onInstall: () => {}, onRecheck: () => {}, onAccount: () => {},
      })), /50 B \/ 100 B/);
      progressHandler?.({ phase: "verify", completedBytes: 100, totalBytes: 100 });
      assert.equal(flow.getState().phase, "verify");
      return { status: "ok", message: "安装完成", installed: true } as T;
    },
    listenProgress: async (handler) => {
      progressHandler = handler;
      return () => { unsubscribed = true; };
    },
  });
  assert.equal(await flow.check(true), false);
  assert.equal(commands.length, 1);
  const dialog = HostInstallDialog({
    state: flow.getState(),
    onClose: () => flow.close(),
    onInstall: () => { void flow.install(); },
    onRecheck: () => { void flow.check(true); },
    onAccount: () => {},
  });
  const html = renderToStaticMarkup(dialog);
  assert.match(html, /启动 Z8 Codex 前，本机需要安装官方 Codex 桌面版。已安装的用户可跳过此步骤；尚未安装请点击“安装”。/);
  assert.match(html, /如果下载速度较慢，建议前往官方渠道或其他正规渠道下载并安装最新版 Codex 桌面版，完成后返回此处重新检测。/);
  assert.match(html, /Z8 提供的官方 Codex Desktop 镜像/);
  assert.match(html, /Windows x64（Intel \/ AMD）/);
  assert.match(html, /Codex-windows-x64\.msix/);
  assert.match(html, /点击下载后检查该平台资源是否已发布/);
  assert.match(html, /下载并安装/);
  assert.equal(commands.length, 1, "rendering never starts a download");

  const installButton = findButton(dialog, "下载并安装");
  assert.ok(installButton);
  installButton.props.onClick();
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.deepEqual(commands.map(([command]) => command), ["z8_host_status", "z8_install_host", "z8_host_status"]);
  assert.equal(flow.getState().phase, "complete");
  assert.equal(flow.getState().host?.installed, true);
  assert.equal(unsubscribed, true);
  let accountAction = 0;
  const readyDialog = HostInstallDialog({
    state: flow.getState(), onClose: () => {}, onInstall: () => {}, onRecheck: () => {}, onAccount: () => { accountAction += 1; },
  });
  assert.match(renderToStaticMarkup(readyDialog), /登录 \/ 创建账户/);
  const accountButton = findButton(readyDialog, "登录 / 创建账户");
  assert.ok(accountButton);
  accountButton.props.onClick();
  assert.equal(accountAction, 1);
});

test("dismissal and unsupported platforms never invoke the installer", async () => {
  const commands: string[] = [];
  const flow = new HostInstallFlow({
    invoke: async <T,>(command: string) => {
      commands.push(command);
      return { ...missingHost, supported: false, platform: "macos-arm64", selectedAsset: null,
        reason: "当前 macOS 版本低于 Codex 桌面版要求。" } as T;
    },
    listenProgress: async () => () => {},
  });
  assert.equal(await flow.check(true), false);
  const dialog = HostInstallDialog({
    state: flow.getState(), onClose: () => flow.close(), onInstall: () => { void flow.install(); },
    onRecheck: () => {}, onAccount: () => {},
  });
  const html = renderToStaticMarkup(dialog);
  assert.match(html, /macOS Apple Silicon/);
  assert.match(html, /macOS 版本低于/);
  assert.equal(findButton(dialog, "下载并安装"), null);
  assert.equal(await flow.install(), false);
  flow.close();
  assert.equal(flow.getState().open, false);
  assert.deepEqual(commands, ["z8_host_status"]);
});

test("failed install stays visible for retry and a pending install cannot be dismissed or duplicated", async () => {
  let finishInstall: ((value: unknown) => void) | undefined;
  let installCalls = 0;
  const flow = new HostInstallFlow({
    invoke: async <T,>(command: string) => {
      if (command === "z8_host_status") return missingHost as T;
      installCalls += 1;
      return new Promise<T>((resolve) => { finishInstall = resolve as (value: unknown) => void; });
    },
    listenProgress: async () => () => {},
  });
  await flow.check(true);
  const running = flow.install();
  assert.equal(await flow.install(), false);
  flow.close();
  assert.equal(flow.getState().open, true);
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(installCalls, 1);
  finishInstall?.({ status: "failed", message: "校验失败", installed: false });
  assert.equal(await running, false);
  assert.equal(flow.getState().phase, "failed");
  assert.match(renderToStaticMarkup(HostInstallDialog({
    state: flow.getState(), onClose: () => {}, onInstall: () => {}, onRecheck: () => {}, onAccount: () => {},
  })), /校验失败.*下载并安装/s);
});

test("busy install shows cancellation and keeps the resume action visible", () => {
  const html = renderToStaticMarkup(HostInstallDialog({
    state: {
      open: true,
      phase: "download",
      host: missingHost,
      progress: { phase: "download", completedBytes: 1024 ** 2, totalBytes: 2 * 1024 ** 2 },
      error: null,
      resumable: true,
      speedBytesPerSecond: 1024 ** 2,
      cancelRequested: false,
    },
    onClose: () => {},
    onInstall: () => {},
    onRecheck: () => {},
    onAccount: () => {},
  }));
  assert.match(html, /取消安装/);
  assert.match(html, /继续安装/);
  assert.match(html, /1\.0 MB\/s/);
  assert.match(html, /disabled/);
});

test("paused install exposes active continue and cancel actions", () => {
  let continued = 0;
  let cancelled = 0;
  const dialog = HostInstallDialog({
    state: {
      open: true,
      phase: "paused",
      host: missingHost,
      progress: null,
      error: "已取消安装，已保留已下载内容，可继续安装。",
      resumable: true,
      speedBytesPerSecond: 0,
      cancelRequested: false,
    },
    onClose: () => {},
    onInstall: () => {},
    onContinue: () => { continued += 1; },
    onCancel: () => { cancelled += 1; },
    onRecheck: () => {},
    onAccount: () => {},
  });
  const continueButton = findButton(dialog, "继续安装");
  const cancelButton = findButton(dialog, "取消安装");
  assert.ok(continueButton);
  assert.ok(cancelButton);
  continueButton.props.onClick();
  cancelButton.props.onClick();
  assert.equal(continued, 1);
  assert.equal(cancelled, 1);
});

test("cancelling a pending install marks it resumable", async () => {
  let finishInstall: ((value: unknown) => void) | undefined;
  const commands: string[] = [];
  const flow = new HostInstallFlow({
    invoke: async <T,>(command: string) => {
      commands.push(command);
      if (command === "z8_host_status") return missingHost as T;
      if (command === "z8_cancel_host_install") {
        return { status: "accepted", message: "正在取消安装", installed: false, resumable: true } as T;
      }
      return new Promise<T>((resolve) => { finishInstall = resolve as (value: unknown) => void; });
    },
    listenProgress: async () => () => {},
  });
  await flow.check(true);
  const installing = flow.install();
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(await flow.cancel(), true);
  assert.equal(flow.getState().cancelRequested, true);
  finishInstall?.({ status: "cancelled", message: "已取消安装", installed: false, resumable: true });
  assert.equal(await installing, false);
  assert.equal(flow.getState().phase, "paused");
  assert.equal(flow.getState().resumable, true);
  assert.deepEqual(commands, ["z8_host_status", "z8_install_host", "z8_cancel_host_install"]);
});

test("recheck after a failed install reaches the ready state without another install", async () => {
  const commands: string[] = [];
  let installed = false;
  const flow = new HostInstallFlow({
    invoke: async <T,>(command: string) => {
      commands.push(command);
      if (command === "z8_host_status") return { ...missingHost, installed } as T;
      return { status: "failed", message: "模拟校验失败", installed: false } as T;
    },
    listenProgress: async () => () => {},
  });
  await flow.check(true);
  assert.equal(await flow.install(), false);
  assert.equal(flow.getState().phase, "failed");
  installed = true;
  let recheck: Promise<boolean> | null = null;
  const dialog = HostInstallDialog({
    state: flow.getState(), onClose: () => {}, onInstall: () => {},
    onRecheck: () => { recheck = flow.check(true); }, onAccount: () => {},
  });
  const button = findButton(dialog, "重新检测");
  assert.ok(button);
  button.props.onClick();
  assert.equal(await recheck, true);
  assert.equal(flow.getState().phase, "complete");
  assert.equal(flow.getState().error, null);
  assert.deepEqual(commands, ["z8_host_status", "z8_install_host", "z8_host_status"]);
  assert.match(renderToStaticMarkup(HostInstallDialog({
    state: flow.getState(), onClose: () => {}, onInstall: () => {}, onRecheck: () => {}, onAccount: () => {},
  })), /登录 \/ 创建账户/);
});

test("post-install host mismatch preserves the native installation guidance", async () => {
  const message = "Windows 安装命令已结束，但当前用户仍未检测到 Codex 桌面版。";
  const flow = new HostInstallFlow({
    invoke: async <T,>(command: string) => {
      if (command === "z8_host_status") return missingHost as T;
      return { status: "ok", installed: true, message } as T;
    },
    listenProgress: async () => () => {},
  });
  await flow.check(true);
  assert.equal(await flow.install(), false);
  assert.equal(flow.getState().phase, "failed");
  assert.equal(flow.getState().error, message);
});

test("dialog keeps an accessible focus target even while installation is busy", () => {
  for (const phase of ["idle", "download", "failed", "complete"] as const) {
    const html = renderToStaticMarkup(HostInstallDialog({
      state: {
        open: true,
        phase,
        host: missingHost,
        progress: null,
        error: null,
        resumable: false,
        speedBytesPerSecond: 0,
        cancelRequested: false,
      },
      onClose: () => {}, onInstall: () => {}, onRecheck: () => {}, onAccount: () => {},
    }));
    assert.match(html, /role="dialog"[^>]*tabindex="-1"/);
    assert.match(html, /aria-modal="true"/);
    assert.match(html, /aria-describedby="z8-host-install-description"/);
    assert.match(html, /id="z8-host-install-description"/);
  }
});

test("status failure blocks launch and never enables an install from stale host data", async () => {
  let fail = false;
  const flow = new HostInstallFlow({
    invoke: async <T,>() => {
      if (fail) throw new Error("private local path");
      return missingHost as T;
    },
    listenProgress: async () => () => {},
  });
  await flow.check(true);
  fail = true;
  assert.equal(await flow.requireHost(""), false);
  assert.equal(flow.getState().phase, "failed");
  assert.doesNotMatch(flow.getState().error ?? "", /private local path/);
  assert.equal(await flow.install(), false);
});

test("closing a pending host check prevents its late response from reopening the dialog", async () => {
  let finishCheck: ((value: unknown) => void) | undefined;
  const flow = new HostInstallFlow({
    invoke: async <T,>() => new Promise<T>((resolve) => { finishCheck = resolve as (value: unknown) => void; }),
    listenProgress: async () => () => {},
  });
  const firstCheck = flow.check(true);
  await new Promise((resolve) => setTimeout(resolve, 0));
  finishCheck?.(missingHost);
  assert.equal(await firstCheck, false);
  const secondCheck = flow.check(true);
  flow.close();
  finishCheck?.(missingHost);
  assert.equal(await secondCheck, false);
  assert.equal(flow.getState().open, false);
});

test("closing before the initial host check resolves prevents the dialog from reopening", async () => {
  let finishCheck: ((value: unknown) => void) | undefined;
  const flow = new HostInstallFlow({
    invoke: async <T,>() => new Promise<T>((resolve) => { finishCheck = resolve as (value: unknown) => void; }),
    listenProgress: async () => () => {},
  });
  const pendingCheck = flow.check(true);
  await new Promise((resolve) => setTimeout(resolve, 0));
  flow.close();
  finishCheck?.(missingHost);
  assert.equal(await pendingCheck, false);
  assert.equal(flow.getState().open, false);
});

test("a transport failure before any bytes does not expose a false continue action", async () => {
  const flow = new HostInstallFlow({
    invoke: async <T,>(command: string) => {
      if (command === "z8_host_status") return missingHost as T;
      throw new Error("network unavailable");
    },
    listenProgress: async () => () => {},
  });
  await flow.check(true);
  assert.equal(await flow.install(), false);
  assert.equal(flow.getState().phase, "failed");
  assert.equal(flow.getState().resumable, false);
  assert.equal(findButton(HostInstallDialog({
    state: flow.getState(), onClose: () => {}, onInstall: () => {}, onRecheck: () => {}, onAccount: () => {},
  }), "继续安装"), null);
});

test("download size formatting uses actual event bytes", () => {
  assert.equal(formatHostDownloadBytes(0), "0 B");
  assert.equal(formatHostDownloadBytes(1024 ** 2), "1.0 MB");
  assert.equal(formatHostDownloadBytes(1024 ** 3), "1.00 GB");
  assert.equal(formatHostDownloadBytes(Number.NaN), "—");
});

test("all four eligible platform payloads display their exact selected asset", async () => {
  const platforms = [
    ["windows-x64", "Windows x64（Intel / AMD）", "Codex-windows-x64.msix"],
    ["windows-arm64", "Windows ARM64", "Codex-windows-arm64.msix"],
    ["macos-x64", "macOS Intel", "Codex-macos-x64.dmg"],
    ["macos-arm64", "macOS Apple Silicon（M 系列）", "Codex-macos-arm64.dmg"],
  ] as const;
  for (const [platform, label, selectedAsset] of platforms) {
    assert.equal(hostPlatformLabel(platform), label);
    const commands: string[] = [];
    const flow = new HostInstallFlow({
      invoke: async <T,>(command: string) => {
        commands.push(command);
        return { ...missingHost, platform, selectedAsset } as T;
      },
      listenProgress: async () => () => {},
    });
    assert.equal(await flow.check(true), false);
    const html = renderToStaticMarkup(HostInstallDialog({
      state: flow.getState(), onClose: () => {}, onInstall: () => {}, onRecheck: () => {}, onAccount: () => {},
    }));
    assert.ok(html.includes(label));
    assert.ok(html.includes(selectedAsset));
    assert.match(html, /下载并安装/);
    assert.deepEqual(commands, ["z8_host_status"], "status rendering cannot download");
  }
});

test("unavailable mirror asset reports server message and never selects another architecture", async () => {
  const commands: string[] = [];
  const flow = new HostInstallFlow({
    invoke: async <T,>(command: string) => {
      commands.push(command);
      if (command === "z8_host_status") return { ...missingHost, platform: "macos-arm64", selectedAsset: "Codex-macos-arm64.dmg" } as T;
      return { status: "failed", installed: false, message: "Z8 镜像尚未提供 macOS Apple Silicon 的安装资源，请稍后重试。" } as T;
    },
    listenProgress: async () => () => {},
  });
  await flow.check(true);
  assert.equal(await flow.install(), false);
  assert.deepEqual(commands, ["z8_host_status", "z8_install_host"]);
  assert.match(flow.getState().error ?? "", /尚未提供 macOS Apple Silicon/);
  assert.equal(flow.getState().host?.selectedAsset, "Codex-macos-arm64.dmg");
});

test("a missing target filename fails closed before install command", async () => {
  const commands: string[] = [];
  const flow = new HostInstallFlow({
    invoke: async <T,>(command: string) => {
      commands.push(command);
      return { ...missingHost, platform: "windows-arm64", selectedAsset: null } as T;
    },
    listenProgress: async () => () => {},
  });
  await flow.check(true);
  const dialog = HostInstallDialog({
    state: flow.getState(), onClose: () => {}, onInstall: () => {}, onRecheck: () => {}, onAccount: () => {},
  });
  const html = renderToStaticMarkup(dialog);
  assert.equal(findButton(dialog, "下载并安装"), null);
  assert.match(html, /无法确认适合本机的镜像资源/);
  assert.equal(await flow.install(), false);
  assert.deepEqual(commands, ["z8_host_status"]);
});
