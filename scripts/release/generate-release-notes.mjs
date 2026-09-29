#!/usr/bin/env node

import fs from "node:fs";
import { execFileSync } from "node:child_process";

function fail(message) {
  console.error(message);
  process.exit(1);
}

function parseArgs(argv) {
  const args = {};
  for (let index = 0; index < argv.length; index += 1) {
    const value = argv[index];
    if (!value.startsWith("--")) {
      fail(`Unexpected argument: ${value}`);
    }
    const key = value.slice(2);
    const next = argv[index + 1];
    if (!next || next.startsWith("--")) {
      fail(`Missing value for --${key}`);
    }
    args[key] = next;
    index += 1;
  }
  return args;
}

function git(...args) {
  try {
    return execFileSync("git", args, { encoding: "utf8" }).trimEnd();
  } catch (error) {
    const detail = error?.stderr?.toString().trim();
    fail(`git ${args.join(" ")} failed${detail ? `: ${detail}` : ""}`);
  }
}

function shellQuote(value) {
  return `\`${String(value).replaceAll("`", "\\`")}\``;
}

const args = parseArgs(process.argv.slice(2));
const tag = args.tag;
const repo = args.repo;
const output = args.output;

if (!tag || !/^v\d+\.\d+\.\d+$/.test(tag)) {
  fail("--tag must use the vX.Y.Z format");
}
if (!repo || !/^[^/]+\/[^/]+$/.test(repo)) {
  fail("--repo must use OWNER/REPOSITORY format");
}
if (!output) {
  fail("--output is required");
}

const tags = git("tag", "--sort=-version:refname", "--list", "v[0-9]*")
  .split("\n")
  .map((value) => value.trim())
  .filter(Boolean);
const currentIndex = tags.indexOf(tag);
const previousTag = currentIndex >= 0 ? tags[currentIndex + 1] ?? null : null;
const range = previousTag ? `${previousTag}..${tag}` : tag;
const log = git("log", "--no-merges", "--format=%H%x09%s", range);
const commits = log
  ? log.split("\n").map((line) => {
      const separator = line.indexOf("\t");
      const hash = separator >= 0 ? line.slice(0, separator) : line;
      const subject = separator >= 0 ? line.slice(separator + 1).trim() : "";
      const body = git("show", "-s", "--format=%B", hash).trim();
      const files = git("show", "--format=", "--name-only", "--no-renames", hash)
        .split("\n")
        .map((value) => value.trim())
        .filter(Boolean);
      return { hash, short: hash.slice(0, 7), subject, body, files };
    })
  : [];

const sectionOrder = [
  "新增功能",
  "修复问题",
  "性能与体验",
  "内部改进",
  "构建与发布",
  "文档与测试",
  "其他变更"
];

function classify(subject) {
  const match = subject.match(/^([a-z]+)(?:\([^)]*\))?!?:\s*(.*)$/i);
  const prefix = match?.[1]?.toLowerCase() ?? "other";
  const category = {
    feat: "新增功能",
    feature: "新增功能",
    fix: "修复问题",
    bugfix: "修复问题",
    perf: "性能与体验",
    optimize: "性能与体验",
    refactor: "内部改进",
    ci: "构建与发布",
    build: "构建与发布",
    chore: "构建与发布",
    release: "构建与发布",
    docs: "文档与测试",
    test: "文档与测试"
  }[prefix] ?? "其他变更";
  return { category, detail: match?.[2]?.trim() ?? subject.trim() };
}

const exactTranslations = [
  [/^automate four-platform releases$/i, "自动化 Windows 与 macOS 四平台的发布流程"],
  [/^match clean release assets$/i, "对齐精简版 Release 资产清单"],
  [/^retry macOS DMG detach$/i, "修复 macOS DMG 分离失败时的重试"],
  [/^remove platform-specific release resource$/i, "移除平台专属发布资源"],
  [/^publish anonymous Z8 Codex source snapshot$/i, "发布匿名 Z8 Codex 源码快照"],
  [/^prepare Z8 Codex v\d+\.\d+\.\d+$/i, "同步 Cargo、管理工具和 Tauri 版本，准备本次发布"]
];

const detailedTranslations = [
  {
    subject: /refresh branding and compact version updates/i,
    files: [/apps\/codex-plus-manager\/src\/App\.tsx$/, /apps\/codex-plus-manager\/src\/styles\.css$/, /assets\/inject\/renderer-inject\.js$/],
    category: "性能与体验",
    items: [
      "版本更新页改为当前版本与最新版本对照，发现新版本时显示 New 标记并提供下载入口。",
      "收窄登录和注册浮窗，移除重复的顶部说明文案，减少桌面窗口中的空白。"
    ]
  },
  {
    subject: /label available updates as new/i,
    files: [/apps\/codex-plus-manager\/src\/App\.tsx$/],
    category: "性能与体验",
    items: ["版本更新卡片使用 New 标记突出显示可用更新，状态更容易识别。"]
  },
  {
    subject: /compact captcha retry and expose diagnostics/i,
    files: [/apps\/codex-plus-manager\/src\/CaptchaWidgets\.tsx$/, /apps\/codex-plus-manager\/src\/account-flow\.ts$/, /apps\/codex-plus-manager\/src-tauri\/src\/z8_commands\.rs$/],
    category: "修复问题",
    items: [
      "安全验证失败、过期或加载异常时显示稳定错误码，并提供就地重试按钮。",
      "安全诊断日志只记录提供商、阶段、错误码和重试信息，不保存 Token、邮箱或服务端错误原文。"
    ]
  },
  {
    subject: /render release notes in update dialog/i,
    files: [/apps\/codex-plus-manager\/src\/App\.tsx$/, /apps\/codex-plus-manager\/src\/styles\.css$/],
    category: "性能与体验",
    items: [
      "更新浮窗解析 Release 的“本次更新”区块，按分类显示新增、修复和体验改进。",
      "隐藏安装包表格、变更链接和原始提交明细，避免 Markdown 原文干扰更新信息。"
    ]
  },
  {
    subject: /stabilize desktop captcha and release checks/i,
    files: [/apps\/codex-plus-manager\/src\/CaptchaWidgets\.tsx$/, /\.github\/workflows\/release-assets\.yml$/, /scripts\/installer\/macos\/package-dmg\.sh$/],
    category: "修复问题",
    items: [
      "桌面端安全验证支持自动重试和过期刷新，并在错误提示中补充当前域名和错误码。",
      "补强发布检查和安装包构建校验，减少平台打包成功但产物不可用的情况。"
    ]
  },
  {
    subject: /close running app during updates/i,
    files: [/apps\/codex-plus-manager\/src-tauri\//],
    category: "修复问题",
    items: ["执行安装包更新前自动关闭正在运行的应用，减少文件占用导致的安装失败。"]
  },
  {
    subject: /retry locked files during installer updates/i,
    files: [/apps\/codex-plus-manager\/src-tauri\//],
    category: "修复问题",
    items: ["更新过程中遇到被占用的文件时自动重试，提高安装包替换的成功率。"]
  },
  {
    subject: /align captcha retry beside widget/i,
    files: [/apps\/codex-plus-manager\/src\/CaptchaWidgets\.tsx$/, /apps\/codex-plus-manager\/src\/z8-brand\.css$/],
    category: "性能与体验",
    items: ["将安全验证重试按钮放到验证组件旁边，失败后可以直接重新验证。"]
  }
];

function bodyNotes(body) {
  return body
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => /^(?:[-*]\s+|release[- ]?note\s*:\s*)/i.test(line))
    .map((line) => line.replace(/^[-*]\s+/, "").replace(/^release[- ]?note\s*:\s*/i, "").trim())
    .filter(Boolean);
}

function hasRequiredFile(commit, patterns) {
  return patterns.some((pattern) => commit.files.some((file) => pattern.test(file)));
}

function summarize(commit) {
  const { category, detail } = classify(commit.subject);
  const detailed = detailedTranslations.find((entry) => entry.subject.test(detail) && hasRequiredFile(commit, entry.files));
  if (detailed) {
    return detailed.items.map((text) => ({ category: detailed.category, text }));
  }
  const exact = exactTranslations.find(([pattern]) => pattern.test(detail));
  if (exact) {
    return [{ category, text: exact[1] }];
  }
  const notes = bodyNotes(commit.body);
  if (notes.length > 0) {
    return notes.map((text) => ({ category, text }));
  }
  if (/[一-鿿]/.test(detail)) {
    return [{ category, text: detail }];
  }
  return [];
}

const groups = new Map(sectionOrder.map((name) => [name, []]));
for (const commit of commits) {
  for (const summary of summarize(commit)) {
    groups.get(summary.category).push(summary.text);
  }
}

const populatedGroups = sectionOrder.filter((name) => groups.get(name).length > 0);
const focus = populatedGroups
  .filter((name) => name !== "其他变更")
  .map((name) => `${groups.get(name).length} 项${name}`)
  .join("、");
const focusText = focus || "未检测到可向用户展示的功能或修复";
const changelogUrl = previousTag
  ? `https://github.com/${repo}/compare/${previousTag}...${tag}`
  : `https://github.com/${repo}/commits/${tag}`;
const changelogText = previousTag
  ? `查看 ${previousTag} 到 ${tag} 的完整变更`
  : `查看 ${tag} 的完整提交记录`;
const version = tag.slice(1);
const releaseBaseUrl = `https://github.com/${repo}/releases/download/${tag}`;
const assetLink = (name) => `[\`${name}\`](${releaseBaseUrl}/${encodeURIComponent(name)})`;

const lines = [
  `## Z8 Codex ${tag}`,
  "",
  `本版本共整理 ${commits.length} 个公开提交，重点包括：${focusText}。`,
  "",
  "## 本次更新"
];

if (populatedGroups.length === 0) {
  lines.push("", "- 本次版本未检测到可向用户展示的新增功能或修复。");
}

for (const name of populatedGroups) {
  lines.push("", `### ${name}`);
  for (const summary of groups.get(name)) {
    lines.push(`- ${summary}`);
  }
}

lines.push(
  "",
  "## 安装包",
  "",
  "请根据电脑芯片选择对应版本。macOS 用户可在“关于本机”中查看芯片类型；DMG 适合普通安装，ZIP 适合手动解压或便携使用。",
  "",
  "| 平台 / 芯片 | 推荐下载 | ZIP 压缩包 |",
  "| --- | --- | --- |",
  `| Windows x64 | ${assetLink(`Z8Codex-${version}-windows-x64-setup.exe`)} | ${assetLink(`Z8Codex-${version}-windows-x64.zip`)} |`,
  `| macOS Intel（x86_64） | ${assetLink(`Z8Codex-${version}-macos-x64.dmg`)} | ${assetLink(`Z8Codex-${version}-macos-x64.zip`)} |`,
  `| macOS Apple Silicon（M1/M2/M3/M4 等） | ${assetLink(`Z8Codex-${version}-macos-arm64.dmg`)} | ${assetLink(`Z8Codex-${version}-macos-arm64.zip`)} |`,
  "",
  "### macOS 芯片选择",
  "",
  "- Intel 芯片：下载文件名中包含 `macos-x64` 的版本。",
  "- Apple Silicon（M1/M2/M3/M4 等）：下载文件名中包含 `macos-arm64` 的版本。",
  "",
  "## 完整变更（Full Changelog）",
  "",
  `[${changelogText}](${changelogUrl})`,
  "",
  "<details>",
  "<summary>查看原始提交明细</summary>",
  ""
);

if (commits.length === 0) {
  lines.push("本次版本没有检测到可展示的提交记录。", "");
} else {
  for (const commit of commits) {
    lines.push(`- ${shellQuote(commit.short)} ${commit.subject || "（无提交说明）"}`);
  }
}

lines.push(
  "</details>",
  "",
  "本版本安装包由 GitHub Actions 自动构建并上传。"
);

fs.writeFileSync(output, `${lines.join("\n")}\n`, "utf8");
console.log(`Generated ${output} for ${tag}${previousTag ? ` from ${previousTag}` : ""}.`);
