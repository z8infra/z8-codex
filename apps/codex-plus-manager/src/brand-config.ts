/**
 * Z8 品牌发行版的单一品牌配置入口。
 *
 * 视觉标记和配色遵循 Z8 Launch DESIGN.md；功能开关保持静态、可审查，
 * 避免上游页面意外露出面向其他发行版的入口。
 */
export const Z8_BRAND = {
  mark: "Z8",
  productName: "Z8 Codex",
  managerName: "Z8 Codex",
  subtitle: "Z8 AI 编程工作台",
  subtitleEn: "Z8 AI coding workspace",
  windowTitle: "Z8 Codex",
  trayTitle: "Z8 Codex",
  websiteUrl: "https://z8.hk/",
  logoAsset: new URL("./assets/z8-logo.png", import.meta.url).href,
} as const;

export const Z8_FEATURES = {
  /** Z8 发行版从 GitHub Release 获取匹配平台的安装包更新。 */
  onlineUpdates: true,
  /** 第一版不开放 Zed Remote 项目入口。 */
  zedRemote: false,
} as const;

export type Z8Feature = keyof typeof Z8_FEATURES;

/**
 * Routes intentionally removed from the first Z8 release. Keep this list
 * separate from the UI route table so an external deep link cannot reopen an
 * upstream-only page after the sidebar item has been hidden.
 */
export const Z8_DISABLED_ROUTES = [
  "grok",
  "zedRemote",
  "recommendations",
] as const;

export function isZ8FeatureEnabled(feature?: Z8Feature): boolean {
  return !feature || Z8_FEATURES[feature];
}

export function isZ8RouteDisabled(route: string): boolean {
  return (Z8_DISABLED_ROUTES as readonly string[]).includes(route);
}

/** Resolve a user supplied route/hash before it reaches React state. */
export function resolveZ8DeepLink(route: string): "overview" | "about" {
  return route === "about" ? "about" : "overview";
}

/** Keep the first-run funnel inside the branded account page. */
export function shouldRequireZ8AccountOnStartup(input: {
  route: string;
  handledNavigation: boolean;
  showUpdate: boolean;
  status: string;
  authenticated: boolean;
}): boolean {
  return input.route === "overview"
    && !input.handledNavigation
    && !input.showUpdate
    && input.status === "ok"
    && !input.authenticated;
}

/**
 * Startup requests run concurrently with the first render. Once the user has
 * navigated away from the initial overview, an older startup response must not
 * take the user back to an automatic page (about/account).
 */
export function canApplyZ8StartupNavigation(input: {
  route: string;
  startupRevision: number;
  currentRevision: number;
}): boolean {
  return input.route === "overview" && input.startupRevision === input.currentRevision;
}
