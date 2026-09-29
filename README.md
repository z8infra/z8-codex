# Z8 Codex

Z8 Codex 是面向 Codex 官方桌面端的账户、供应商与启动管理工具。它以官方桌面应用为会话宿主，在本机管理 Z8 账户、API Key、Provider 配置、用量信息和启动流程，并提供可选的 Codex 界面增强功能。

<p align="center">
  <img src="apps/codex-plus-manager/src/assets/z8-logo.png" alt="Z8 Codex 图标" width="120">
</p>

<p align="center">
  <a href="https://z8.hk/">Z8 官网</a> ·
  <a href="https://github.com/z8infra/z8-codex">项目仓库</a> ·
  <a href="https://github.com/BigPizzaV3/CodexPlusPlus">上游项目</a> ·
  <a href="LICENSE">AGPL-3.0-only</a>
</p>

## 项目来源与许可

本项目基于 [BigPizzaV3/CodexPlusPlus](https://github.com/BigPizzaV3/CodexPlusPlus) 二次开发，遵循 GNU Affero General Public License v3.0（SPDX：`AGPL-3.0-only`）。Z8 Codex 在上游基础上进行了品牌、账户体系、供应商配置、用量展示、桌面端安装与启动流程等修改。

分发本项目时，请一并保留 [LICENSE](LICENSE) 和 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)，并按照 AGPL-3.0-only 提供对应版本的源代码。修改内容以当前源代码和提交历史为准。

Z8 Codex 与 OpenAI、ChatGPT、Codex 没有隶属、背书或官方授权关系。OpenAI、ChatGPT、Codex 及相关标识归其各自权利人所有。

## 功能概览

### Z8 账户

- 登录、注册、退出和账户状态管理。
- 管理多个 API Key，选择后写入 Z8 Provider。
- 查看可用余额、今日请求、Token 用量和累计消耗（以服务端返回为准）。
- 兑换额度并跳转 Z8 官方充值页面。

### 供应商与模型

- 配置纯 API、官方登录混入 API 和聚合供应商。
- 支持 Responses API 与 Chat Completions 协议。
- 配置 Base URL、API Key、模型列表、测试模型、上下文窗口和自动压缩阈值。
- 支持供应商健康检查、模型测试、故障转移、轮转和权重路由。

### Codex 桌面端管理

- 检测本机 Codex 桌面端并保存应用路径。
- 下载或选择本地 Codex 桌面端镜像。
- 启动、重启和诊断 Codex，同时避免重复启动同一实例。
- 将账户和供应商设置同步到 Codex 所需的本地配置。

### 可选增强

- 会话扫描、批量删除、Markdown 导出和 Token 用量历史。
- 插件、Skill、MCP、模型白名单、粘贴修复和强制中文界面。
- 会话宽度、滚动位置、线程 ID、服务层级和 Goals 等界面增强。
- 日志、健康检查、配置备份和故障诊断。

增强功能可以在管理工具中单独关闭。关闭增强后，Z8 Codex 仍可作为账户、供应商和启动管理工具使用。

## 安装与首次使用

### 安装官方 Codex 桌面端

Z8 Codex 需要本机已经安装官方 Codex 桌面端才能启动会话。首次打开管理工具后，请先进入“安装维护”检查应用路径和版本；如果尚未安装，可以从 Z8 提供的镜像下载安装，或使用官方渠道安装后重新检测。

### 安装 Z8 Codex

发布包按系统和处理器架构提供：

- Windows x64：安装程序 `Z8Codex-*-windows-x64-setup.exe` 和 ZIP
- macOS Intel：DMG 和 ZIP（`Z8Codex-*-macos-x64.*`）
- macOS Apple Silicon：DMG 和 ZIP（`Z8Codex-*-macos-arm64.*`）

安装后打开“Z8 Codex 管理工具”，按以下顺序完成设置：

1. 登录或注册 Z8 账户。
2. 选择 API Key，并确认 Z8 Provider 连接正常。
3. 检查账户用量和本机 Codex 桌面端路径。
4. 点击“启动 Codex”进入会话。

低配置设备首次启动可能需要更长时间。启动按钮在任务完成前会暂时锁定，避免重复打开多个 Codex 实例。

## 本地数据

Z8 Codex 使用本机用户目录保存配置、缓存、日志和备份。常见位置包括：

- Codex 配置：`~/.codex/config.toml`
- Codex 登录状态：`~/.codex/auth.json`
- Codex 数据库：优先使用 `~/.codex/sqlite/*.db`，旧版本回退到 `~/.codex/state_5.sqlite`
- Z8 Codex 状态与日志：`~/.codex-session-delete/`
- Provider 同步备份：`~/.codex/backups_state/provider-sync`

API Key 和登录信息属于敏感数据。请勿把配置文件、日志、截图或导出的诊断信息发布到公开渠道。

## 从源码构建

### 通用依赖

- Node.js 22 或兼容版本
- npm
- Rust stable toolchain
- Git

### Windows

安装 NSIS 后执行：

```powershell
npm ci --prefix apps/codex-plus-manager
npm run vite:build --prefix apps/codex-plus-manager
cargo build --release --locked --target x86_64-pc-windows-msvc
```

安装包脚本位于 `scripts/installer/windows/CodexPlusPlus.nsi`。构建 ARM64 时将 Rust target 和安装器架构参数改为 `aarch64-pc-windows-msvc` / `arm64`。

### macOS

macOS 安装包需要在 macOS 原生环境中构建，并使用 Xcode Command Line Tools 提供的 `codesign`、`hdiutil`、`lipo`、`sips` 和 `iconutil` 等工具：

```bash
npm ci --prefix apps/codex-plus-manager
npm run vite:build --prefix apps/codex-plus-manager
cargo build --release --locked --target x86_64-apple-darwin
MACOS_BUILD_NUMBER=local BINARY_DIR="$PWD/target/x86_64-apple-darwin/release" \
  bash scripts/installer/macos/package-dmg.sh 1.3.2 x64
```

Apple Silicon 使用 `aarch64-apple-darwin` 和 `arm64` 参数。内部测试包使用临时签名，不代表 Developer ID 签名或公证发布包。

### 自动发版

GitHub Actions 会在推送严格的版本标签（例如 `v1.3.3`）后自动创建 GitHub Release，并发布六个下载包：Windows x64 的安装程序和 ZIP、macOS Intel 的 DMG 和 ZIP、macOS Apple Silicon 的 DMG 和 ZIP，同时上传 `latest.json`。GitHub 会自动提供对应提交的 Source code ZIP 和 tar.gz。

发版前请把以下三个版本号同步为同一个 `X.Y.Z`：

- 根目录 `Cargo.toml` 的 workspace version
- `apps/codex-plus-manager/package.json` 的 version
- `apps/codex-plus-manager/src-tauri/tauri.conf.json` 的 version

然后提交并推送代码，再推送版本标签：

```bash
git tag v1.3.3
git push origin v1.3.3
```

工作流会校验标签和三个版本号；版本不一致时会停止，不会发布不匹配的安装包。Windows 安装包目前未配置代码签名，macOS 安装包需要后续配置 Apple Developer ID 签名和公证凭据后才能作为正式签名版本分发。

## 常见问题

### 启动按钮没有反应

先在“安装维护”页面确认 Codex 桌面端路径有效，并关闭已经运行的 Codex 后重试。管理工具会检查现有实例、启动状态和调试端口，避免重复启动。

### 供应商连接正常但请求失败

在供应商详情中运行健康检查和模型测试，确认协议、Base URL、API Key 与测试模型匹配。Responses API 与 Chat Completions 的配置不能混用。

### macOS 提示无法打开

首次运行未签名的内部测试包时，可能需要在“系统设置 → 隐私与安全性”中允许打开。正式分发前应使用 Developer ID 签名并完成 Apple 公证。

## 相关文件

- [上游 CodexPlusPlus](https://github.com/BigPizzaV3/CodexPlusPlus)
- [GNU AGPL v3.0 许可证](LICENSE)
- [第三方组件声明](THIRD_PARTY_NOTICES.md)
- [Z8 官网](https://z8.hk/)
