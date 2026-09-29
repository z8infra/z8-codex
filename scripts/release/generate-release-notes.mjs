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
      return { hash, short: hash.slice(0, 7), subject };
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

function summarize(commit) {
  const { category, detail } = classify(commit.subject);
  const exact = exactTranslations.find(([pattern]) => pattern.test(detail));
  if (exact) {
    return { category, text: exact[1] };
  }
  if (/[一-鿿]/.test(detail)) {
    return { category, text: detail };
  }
  const fallback = {
    "新增功能": "补充用户可见功能",
    "修复问题": "修复已发现的问题",
    "性能与体验": "改善运行性能与使用体验",
    "内部改进": "整理核心实现",
    "构建与发布": "完善构建与发布流程",
    "文档与测试": "补充文档和回归验证",
    "其他变更": "完成其他相关调整"
  }[category];
  return { category, text: `${fallback}（提交 ${commit.short}）` };
}

const groups = new Map(sectionOrder.map((name) => [name, []]));
for (const commit of commits) {
  const summary = summarize(commit);
  groups.get(summary.category).push(summary.text);
}

const populatedGroups = sectionOrder.filter((name) => groups.get(name).length > 0);
const focus = populatedGroups
  .filter((name) => name !== "其他变更")
  .map((name) => `${groups.get(name).length} 项${name}`)
  .join("、");
const focusText = focus || "常规维护与稳定性改进";
const changelogUrl = previousTag
  ? `https://github.com/${repo}/compare/${previousTag}...${tag}`
  : `https://github.com/${repo}/commits/${tag}`;
const changelogText = previousTag
  ? `查看 ${previousTag} 到 ${tag} 的完整变更`
  : `查看 ${tag} 的完整提交记录`;

const lines = [
  `## Z8 Codex ${tag}`,
  "",
  `本版本共整理 ${commits.length} 个公开提交，重点包括：${focusText}。`,
  "",
  "## 本次更新"
];

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
  "- Windows x64：安装程序（.exe）和 ZIP 压缩包",
  "- macOS Intel：DMG 和 ZIP 压缩包",
  "- macOS Apple Silicon：DMG 和 ZIP 压缩包",
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
