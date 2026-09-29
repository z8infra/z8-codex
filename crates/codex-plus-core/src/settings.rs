use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;
use serde_json::{Map, Value};
use toml_edit::{DocumentMut, Item};

use crate::tools::{ToolConfig, ToolId};
use crate::zed_remote::ZedOpenStrategy;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LaunchMode {
    #[default]
    Patch,
    Relay,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayProfile {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing)]
    pub model: String,
    #[serde(default = "default_relay_base_url", skip_serializing)]
    pub base_url: String,
    #[serde(rename = "upstreamBaseUrl", default)]
    pub upstream_base_url: String,
    #[serde(
        default,
        skip_serializing,
        deserialize_with = "deserialize_profile_api_key"
    )]
    pub api_key: String,
    #[serde(default)]
    pub protocol: RelayProtocol,
    #[serde(rename = "relayMode", default)]
    pub relay_mode: RelayMode,
    #[serde(rename = "officialMixApiKey", default)]
    pub official_mix_api_key: bool,
    #[serde(rename = "noAuth", default)]
    pub no_auth: bool,
    #[serde(rename = "hideOfficialUsageAlert", default)]
    pub hide_official_usage_alert: bool,
    #[serde(rename = "testModel", default)]
    pub test_model: String,
    #[serde(rename = "configContents", default)]
    pub config_contents: String,
    #[serde(rename = "authContents", default)]
    pub auth_contents: String,
    #[serde(rename = "useCommonConfig", default = "default_true")]
    pub use_common_config: bool,
    #[serde(rename = "contextWindow", default)]
    pub context_window: String,
    #[serde(rename = "autoCompactLimit", default)]
    pub auto_compact_limit: String,
    #[serde(rename = "modelInsertMode", default)]
    pub model_insert_mode: RelayModelInsertMode,
    #[serde(rename = "modelList", default)]
    pub model_list: String,
    #[serde(
        rename = "modelWindows",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub model_windows: String,
    /// 每模型自动压缩百分比（JSON map: slug -> 百分比字符串，如 "90" 或 "90%"）。
    /// 为空时保持 Codex 原有的默认自动压缩行为，不向 catalog 写入覆盖值。
    #[serde(
        rename = "modelAutoCompact",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub model_auto_compact: String,
    /// 每模型元数据覆盖（JSON map: slug -> 字段覆盖）。
    #[serde(
        rename = "modelMetadata",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub model_metadata: String,
    #[serde(rename = "modelVlm", default, skip_serializing_if = "String::is_empty")]
    pub model_vlm: String,
    #[serde(
        rename = "vlmApiKey",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub vlm_api_key: String,
    #[serde(rename = "vlmModel", default)]
    pub vlm_model: String,
    #[serde(rename = "vlmBaseUrl", default)]
    pub vlm_base_url: String,
    #[serde(
        rename = "userAgent",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub user_agent: String,
    #[serde(rename = "sub2apiEnabled", default)]
    pub sub2api_enabled: bool,
    #[serde(
        rename = "sub2apiMultiplier",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub sub2api_multiplier: String,
    #[serde(rename = "modelRoutes", default, skip_serializing_if = "Vec::is_empty")]
    pub model_routes: Vec<RelayModelRoute>,
    // 上游审查要求：导出 round-trip 不改变既有 provider；为 false 时不写出该字段。
    #[serde(rename = "standardOpenaiProtocol", default, skip_serializing_if = "is_false")]
    pub standard_openai_protocol: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayModelRoute {
    pub model: String,
    #[serde(rename = "targetRelayId")]
    pub target_relay_id: String,
    #[serde(
        rename = "targetModel",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub target_model: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum AggregateRelayStrategy {
    #[default]
    Failover,
    ConversationRoundRobin,
    RequestRoundRobin,
    WeightedRoundRobin,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AggregateRelayMember {
    #[serde(rename = "relayId")]
    pub relay_id: String,
    #[serde(default = "default_aggregate_member_weight")]
    pub weight: u32,
}

/// 聚合供应商按模型名路由规则：model 匹配 pattern 时转发到指定成员
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AggregateRelayRoute {
    /// 模型匹配模式，如 "deepseek-*" / "gpt-*" / "*"；仅支持 * 通配符
    pub pattern: String,
    /// 目标聚合成员 relayId（必须是本聚合 members 之一）
    #[serde(rename = "relayId")]
    pub relay_id: String,
    /// 数字越大越优先，缺省 0；同 priority 按数组顺序（稳定优先）
    #[serde(default)]
    pub priority: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RelaySessionProvider {
    #[default]
    Custom,
    Openai,
}

impl RelaySessionProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Custom => "custom",
            Self::Openai => "openai",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AggregateRelayProfile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub session_provider: RelaySessionProvider,
    #[serde(default)]
    pub strategy: AggregateRelayStrategy,
    #[serde(default)]
    pub members: Vec<AggregateRelayMember>,
    #[serde(default)]
    pub routes: Vec<AggregateRelayRoute>,
}

impl Default for RelayProfile {
    fn default() -> Self {
        Self {
            id: "default".to_string(),
            name: "默认中转".to_string(),
            model: String::new(),
            base_url: default_relay_base_url(),
            upstream_base_url: String::new(),
            api_key: String::new(),
            protocol: RelayProtocol::Responses,
            relay_mode: RelayMode::Official,
            official_mix_api_key: false,
            no_auth: false,
            hide_official_usage_alert: false,
            test_model: String::new(),
            config_contents: String::new(),
            auth_contents: String::new(),
            use_common_config: true,
            context_window: String::new(),
            auto_compact_limit: String::new(),
            model_insert_mode: RelayModelInsertMode::Patch,
            model_list: String::new(),
            model_windows: String::new(),
            model_auto_compact: String::new(),
            model_metadata: String::new(),
            model_vlm: String::new(),
            vlm_api_key: String::new(),
            vlm_model: String::new(),
            vlm_base_url: String::new(),
            user_agent: String::new(),
            sub2api_enabled: false,
            sub2api_multiplier: String::new(),
            model_routes: Vec::new(),
            standard_openai_protocol: false,
        }
    }
}

impl RelayProfile {
    pub fn uses_no_auth(&self) -> bool {
        self.relay_mode == RelayMode::PureApi && self.no_auth
    }

    pub fn has_model_routes(&self) -> bool {
        self.model_routes
            .iter()
            .any(|route| !route.model.trim().is_empty() && !route.target_relay_id.trim().is_empty())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum RelayModelInsertMode {
    ModelCatalog,
    #[default]
    Patch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum RelayProtocol {
    #[default]
    Responses,
    ChatCompletions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum RelayMode {
    Official,
    #[default]
    MixedApi,
    PureApi,
    Aggregate,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BackendSettings {
    #[serde(rename = "codexAppPath", default)]
    pub codex_app_path: String,
    #[serde(rename = "codexExtraArgs", default)]
    pub codex_extra_args: Vec<String>,
    #[serde(rename = "providerSyncEnabled", default)]
    pub provider_sync_enabled: bool,
    #[serde(rename = "providerSyncSavedProviders", default)]
    pub provider_sync_saved_providers: Vec<String>,
    #[serde(rename = "providerSyncManualProviders", default)]
    pub provider_sync_manual_providers: Vec<String>,
    #[serde(rename = "providerSyncLastSelectedProvider", default)]
    pub provider_sync_last_selected_provider: String,
    #[serde(rename = "ccsDbPath", default)]
    pub ccs_db_path: String,
    #[serde(rename = "relayProfilesEnabled", default = "default_true")]
    pub relay_profiles_enabled: bool,
    #[serde(rename = "enhancementsEnabled", default = "default_true")]
    pub enhancements_enabled: bool,
    #[serde(rename = "codexAppPluginMarketplaceUnlock", default = "default_true")]
    pub codex_app_plugin_marketplace_unlock: bool,
    #[serde(rename = "codexAppModelWhitelistUnlock", default = "default_true")]
    pub codex_app_model_whitelist_unlock: bool,
    #[serde(rename = "codexAppSessionDelete", default = "default_true")]
    pub codex_app_session_delete: bool,
    #[serde(rename = "codexAppMarkdownExport", default = "default_true")]
    pub codex_app_markdown_export: bool,
    #[serde(rename = "codexAppPasteFix", default)]
    pub codex_app_paste_fix: bool,
    #[serde(rename = "codexAppForceChineseLocale", default = "default_true")]
    pub codex_app_force_chinese_locale: bool,
    #[serde(rename = "codexAppFastStartup", default)]
    pub codex_app_fast_startup: bool,
    #[serde(rename = "codexAppThreadIdBadge", default)]
    pub codex_app_thread_id_badge: bool,
    #[serde(rename = "codexAppConversationView", default)]
    pub codex_app_conversation_view: bool,
    #[serde(rename = "codexAppThreadScrollRestore", default = "default_true")]
    pub codex_app_thread_scroll_restore: bool,
    #[serde(rename = "codexAppZedRemoteOpen", default = "default_true")]
    pub codex_app_zed_remote_open: bool,
    #[serde(rename = "zedRemoteOpenStrategy", default)]
    pub zed_remote_open_strategy: ZedOpenStrategy,
    #[serde(rename = "zedRemoteProjectRegistryEnabled", default = "default_true")]
    pub zed_remote_project_registry_enabled: bool,
    #[serde(rename = "zedRemoteSyncToZedSettings", default)]
    pub zed_remote_sync_to_zed_settings: bool,
    #[serde(rename = "codexAppUpstreamWorktreeCreate", default = "default_true")]
    pub codex_app_upstream_worktree_create: bool,
    #[serde(rename = "codexAppNativeMenuPlacement", default = "default_true")]
    pub codex_app_native_menu_placement: bool,
    #[serde(rename = "codexAppNativeMenuLocalization", default = "default_true")]
    pub codex_app_native_menu_localization: bool,
    #[serde(rename = "codexAppNativeBrowserRequireIdentification", default)]
    pub codex_app_native_browser_require_identification: bool,
    #[serde(rename = "codexAppServiceTierControls", default)]
    pub codex_app_service_tier_controls: bool,
    #[serde(rename = "codexAppPetRealMouseLook", default)]
    pub codex_app_pet_real_mouse_look: bool,
    #[serde(rename = "codexAppStepwiseEnabled", default)]
    pub codex_app_stepwise_enabled: bool,
    #[serde(
        rename = "codexAppStepwiseGenerationMode",
        default = "default_stepwise_generation_mode",
        deserialize_with = "deserialize_stepwise_generation_mode"
    )]
    pub codex_app_stepwise_generation_mode: String,
    #[serde(rename = "codexAppAnswerOutlineEnabled", default)]
    pub codex_app_answer_outline_enabled: bool,
    #[serde(rename = "codexAppStepwiseDirectSend", default)]
    pub codex_app_stepwise_direct_send: bool,
    #[serde(rename = "codexAppStepwiseBaseUrl", default)]
    pub codex_app_stepwise_base_url: String,
    #[serde(rename = "codexAppStepwiseApiKey", default)]
    pub codex_app_stepwise_api_key: String,
    #[serde(
        rename = "codexAppStepwiseApiKeyEnv",
        default = "default_stepwise_api_key_env",
        deserialize_with = "empty_as_default_stepwise_api_key_env"
    )]
    pub codex_app_stepwise_api_key_env: String,
    #[serde(
        rename = "codexAppStepwiseProtocol",
        default = "default_stepwise_protocol",
        deserialize_with = "deserialize_stepwise_protocol"
    )]
    pub codex_app_stepwise_protocol: String,
    #[serde(rename = "codexAppStepwiseModel", default)]
    pub codex_app_stepwise_model: String,
    #[serde(
        rename = "codexAppStepwiseMaxItems",
        default = "default_stepwise_max_items",
        deserialize_with = "deserialize_stepwise_max_items"
    )]
    pub codex_app_stepwise_max_items: u8,
    #[serde(
        rename = "codexAppStepwiseMaxInputChars",
        default = "default_stepwise_max_input_chars",
        deserialize_with = "deserialize_stepwise_max_input_chars"
    )]
    pub codex_app_stepwise_max_input_chars: u32,
    #[serde(
        rename = "codexAppStepwiseMaxOutputTokens",
        default = "default_stepwise_max_output_tokens",
        deserialize_with = "deserialize_stepwise_max_output_tokens"
    )]
    pub codex_app_stepwise_max_output_tokens: u32,
    #[serde(
        rename = "codexAppStepwiseTimeoutMs",
        default = "default_stepwise_timeout_ms",
        deserialize_with = "deserialize_stepwise_timeout_ms"
    )]
    pub codex_app_stepwise_timeout_ms: u64,
    #[serde(rename = "codexAppImageOverlayEnabled", default)]
    pub codex_app_image_overlay_enabled: bool,
    #[serde(rename = "codexAppImageOverlayPath", default)]
    pub codex_app_image_overlay_path: String,
    #[serde(
        rename = "codexAppImageOverlayOpacity",
        default = "default_image_overlay_opacity",
        deserialize_with = "deserialize_image_overlay_opacity"
    )]
    pub codex_app_image_overlay_opacity: u8,
    #[serde(
        rename = "codexAppImageOverlayFitMode",
        default = "default_image_overlay_fit_mode",
        deserialize_with = "deserialize_image_overlay_fit_mode"
    )]
    pub codex_app_image_overlay_fit_mode: String,
    #[serde(rename = "codexGoalsEnabled", default)]
    pub codex_goals_enabled: bool,
    #[serde(rename = "weixinConnectEnabled", default)]
    pub weixin_connect_enabled: bool,
    #[serde(
        rename = "weixinConnectBaseUrl",
        default = "default_weixin_connect_base_url"
    )]
    pub weixin_connect_base_url: String,
    #[serde(rename = "weixinConnectToken", default)]
    pub weixin_connect_token: String,
    #[serde(rename = "weixinConnectAccountId", default)]
    pub weixin_connect_account_id: String,
    #[serde(rename = "weixinConnectAllowFrom", default)]
    pub weixin_connect_allow_from: String,
    #[serde(rename = "weixinConnectRouteTag", default)]
    pub weixin_connect_route_tag: String,
    #[serde(rename = "weixinConnectWorkDir", default)]
    pub weixin_connect_work_dir: String,
    #[serde(rename = "weixinConnectModel", default)]
    pub weixin_connect_model: String,
    #[serde(
        rename = "weixinConnectSandbox",
        default = "default_weixin_connect_sandbox"
    )]
    pub weixin_connect_sandbox: String,
    #[serde(rename = "weixinConnectCodexPath", default)]
    pub weixin_connect_codex_path: String,
    #[serde(rename = "launchMode", default)]
    pub launch_mode: LaunchMode,
    #[serde(rename = "relayBaseUrl", default = "default_relay_base_url")]
    pub relay_base_url: String,
    #[serde(rename = "relayApiKey", default)]
    pub relay_api_key: String,
    #[serde(rename = "relayProfiles", default = "default_relay_profiles")]
    pub relay_profiles: Vec<RelayProfile>,
    #[serde(rename = "relayCommonConfigContents", default)]
    pub relay_common_config_contents: String,
    #[serde(rename = "relayContextConfigContents", default)]
    pub relay_context_config_contents: String,
    #[serde(rename = "activeRelayId", default = "default_active_relay_id")]
    pub active_relay_id: String,
    #[serde(rename = "aggregateRelayProfiles", default)]
    pub aggregate_relay_profiles: Vec<AggregateRelayProfile>,
    #[serde(rename = "activeAggregateRelayId", default)]
    pub active_aggregate_relay_id: String,
    #[serde(rename = "relayTestModel", default = "default_relay_test_model")]
    pub relay_test_model: String,
    /// 按工具分区的配置。`tools.codex` 是上面那批扁平字段的镜像（写盘时同步），
    /// 其它工具（Grok / 后续工具）只存在这里。见 `crate::tools`。
    #[serde(rename = "tools", default)]
    pub tools: BTreeMap<ToolId, ToolConfig>,
    /// UI 顶栏当前聚焦的工具。只影响管理器的展示，不影响 Codex 的启动配置。
    #[serde(rename = "activeTool", default)]
    pub active_tool: ToolId,
}

impl Default for BackendSettings {
    fn default() -> Self {
        Self {
            codex_app_path: String::new(),
            codex_extra_args: Vec::new(),
            provider_sync_enabled: false,
            provider_sync_saved_providers: Vec::new(),
            provider_sync_manual_providers: Vec::new(),
            provider_sync_last_selected_provider: String::new(),
            ccs_db_path: String::new(),
            relay_profiles_enabled: true,
            enhancements_enabled: true,
            codex_app_plugin_marketplace_unlock: true,
            codex_app_model_whitelist_unlock: true,
            codex_app_session_delete: true,
            codex_app_markdown_export: true,
            codex_app_paste_fix: false,
            codex_app_force_chinese_locale: true,
            codex_app_fast_startup: false,
            codex_app_thread_id_badge: false,
            codex_app_conversation_view: false,
            codex_app_thread_scroll_restore: true,
            codex_app_zed_remote_open: true,
            zed_remote_open_strategy: ZedOpenStrategy::AddToFocusedWorkspace,
            zed_remote_project_registry_enabled: true,
            zed_remote_sync_to_zed_settings: false,
            codex_app_upstream_worktree_create: true,
            codex_app_native_menu_placement: true,
            codex_app_native_menu_localization: true,
            codex_app_native_browser_require_identification: false,
            codex_app_service_tier_controls: false,
            codex_app_pet_real_mouse_look: false,
            codex_app_stepwise_enabled: false,
            codex_app_stepwise_generation_mode: default_stepwise_generation_mode(),
            codex_app_answer_outline_enabled: false,
            codex_app_stepwise_direct_send: false,
            codex_app_stepwise_base_url: String::new(),
            codex_app_stepwise_api_key: String::new(),
            codex_app_stepwise_api_key_env: default_stepwise_api_key_env(),
            codex_app_stepwise_protocol: default_stepwise_protocol(),
            codex_app_stepwise_model: String::new(),
            codex_app_stepwise_max_items: default_stepwise_max_items(),
            codex_app_stepwise_max_input_chars: default_stepwise_max_input_chars(),
            codex_app_stepwise_max_output_tokens: default_stepwise_max_output_tokens(),
            codex_app_stepwise_timeout_ms: default_stepwise_timeout_ms(),
            codex_app_image_overlay_enabled: false,
            codex_app_image_overlay_path: String::new(),
            codex_app_image_overlay_opacity: default_image_overlay_opacity(),
            codex_app_image_overlay_fit_mode: default_image_overlay_fit_mode(),
            codex_goals_enabled: false,
            weixin_connect_enabled: false,
            weixin_connect_base_url: default_weixin_connect_base_url(),
            weixin_connect_token: String::new(),
            weixin_connect_account_id: String::new(),
            weixin_connect_allow_from: String::new(),
            weixin_connect_route_tag: String::new(),
            weixin_connect_work_dir: String::new(),
            weixin_connect_model: String::new(),
            weixin_connect_sandbox: default_weixin_connect_sandbox(),
            weixin_connect_codex_path: String::new(),
            launch_mode: LaunchMode::Patch,
            relay_base_url: default_relay_base_url(),
            relay_api_key: String::new(),
            relay_profiles: default_relay_profiles(),
            relay_common_config_contents: String::new(),
            relay_context_config_contents: String::new(),
            active_relay_id: default_active_relay_id(),
            aggregate_relay_profiles: Vec::new(),
            active_aggregate_relay_id: String::new(),
            relay_test_model: default_relay_test_model(),
            tools: BTreeMap::new(),
            active_tool: ToolId::Codex,
        }
    }
}

impl BackendSettings {
    pub fn active_relay_profile(&self) -> RelayProfile {
        if self.active_relay_id == default_active_relay_id()
            && self.relay_profiles.len() == 1
            && self.relay_profiles[0] == RelayProfile::default()
            && (!self.relay_api_key.is_empty() || self.relay_base_url != default_relay_base_url())
        {
            return RelayProfile {
                id: default_active_relay_id(),
                name: "默认中转".to_string(),
                model: String::new(),
                base_url: if self.relay_base_url.is_empty() {
                    default_relay_base_url()
                } else {
                    self.relay_base_url.clone()
                },
                upstream_base_url: if self.relay_base_url.is_empty() {
                    default_relay_base_url()
                } else {
                    self.relay_base_url.clone()
                },
                api_key: self.relay_api_key.clone(),
                protocol: RelayProtocol::Responses,
                relay_mode: RelayMode::MixedApi,
                official_mix_api_key: true,
                no_auth: false,
                hide_official_usage_alert: false,
                test_model: String::new(),
                config_contents: String::new(),
                auth_contents: String::new(),
                use_common_config: true,
                context_window: String::new(),
                auto_compact_limit: String::new(),
                model_insert_mode: RelayModelInsertMode::Patch,
                model_list: String::new(),
                model_windows: String::new(),
                model_auto_compact: String::new(),
                model_metadata: String::new(),
                model_vlm: String::new(),
                vlm_api_key: String::new(),
                vlm_model: String::new(),
                vlm_base_url: String::new(),
                user_agent: String::new(),
                sub2api_enabled: false,
                sub2api_multiplier: String::new(),
                model_routes: Vec::new(),
                standard_openai_protocol: false,
            };
        }

        if let Some(profile) = self
            .relay_profiles
            .iter()
            .find(|profile| profile.id == self.active_relay_id)
        {
            return profile.clone();
        }

        RelayProfile {
            id: if self.active_relay_id.is_empty() {
                default_active_relay_id()
            } else {
                self.active_relay_id.clone()
            },
            name: "默认中转".to_string(),
            model: String::new(),
            base_url: if self.relay_base_url.is_empty() {
                default_relay_base_url()
            } else {
                self.relay_base_url.clone()
            },
            upstream_base_url: if self.relay_base_url.is_empty() {
                default_relay_base_url()
            } else {
                self.relay_base_url.clone()
            },
            api_key: self.relay_api_key.clone(),
            protocol: RelayProtocol::Responses,
            relay_mode: RelayMode::Official,
            official_mix_api_key: false,
            no_auth: false,
            hide_official_usage_alert: false,
            test_model: String::new(),
            config_contents: String::new(),
            auth_contents: String::new(),
            use_common_config: true,
            context_window: String::new(),
            auto_compact_limit: String::new(),
            model_insert_mode: RelayModelInsertMode::Patch,
            model_list: String::new(),
            model_windows: String::new(),
            model_auto_compact: String::new(),
            model_metadata: String::new(),
            model_vlm: String::new(),
            vlm_api_key: String::new(),
            vlm_model: String::new(),
            vlm_base_url: String::new(),
            user_agent: String::new(),
            sub2api_enabled: false,
            sub2api_multiplier: String::new(),
            model_routes: Vec::new(),
            standard_openai_protocol: false,
        }
    }

    pub fn active_aggregate_relay_profile(&self) -> Option<AggregateRelayProfile> {
        let active_relay = self
            .relay_profiles
            .iter()
            .find(|profile| profile.id == self.active_relay_id)?;
        if active_relay.relay_mode != RelayMode::Aggregate {
            return None;
        }

        let active_aggregate_id = if self.active_aggregate_relay_id.trim().is_empty() {
            active_relay.id.as_str()
        } else {
            self.active_aggregate_relay_id.trim()
        };

        if active_aggregate_id != active_relay.id {
            return None;
        }

        self.aggregate_relay_profiles
            .iter()
            .find(|profile| profile.id == active_aggregate_id)
            .cloned()
    }

    pub fn active_relay_session_provider(&self) -> RelaySessionProvider {
        if let Some(profile) = self.active_aggregate_relay_profile() {
            return profile.session_provider;
        }
        if self
            .active_relay_profile()
            .config_contents
            .parse::<DocumentMut>()
            .ok()
            .is_some_and(|doc| {
                doc.get("model_provider")
                    .and_then(Item::as_str)
                    .map(str::trim)
                    .is_some_and(|provider| provider == "openai")
            })
        {
            RelaySessionProvider::Openai
        } else {
            RelaySessionProvider::Custom
        }
    }

    pub fn active_relay_transport_uses_protocol_proxy(&self) -> bool {
        self.active_aggregate_relay_profile().is_some()
            || self.active_relay_profile().protocol == RelayProtocol::ChatCompletions
            || self.active_relay_profile().has_model_routes()
            || self.active_relay_profile().uses_no_auth()
    }

    pub fn active_relay_uses_protocol_proxy(&self) -> bool {
        self.active_relay_transport_uses_protocol_proxy()
            || self.active_relay_session_provider() == RelaySessionProvider::Openai
    }
}

pub fn default_stepwise_api_key_env() -> String {
    "CODEX_STEPWISE_API_KEY".to_string()
}

pub fn default_stepwise_protocol() -> String {
    "chat_completions".to_string()
}

pub fn normalize_stepwise_protocol(value: &str) -> String {
    match value.trim() {
        "chat_completions" | "responses" | "anthropic_messages" | "auto" => {
            value.trim().to_string()
        }
        _ => default_stepwise_protocol(),
    }
}

pub fn default_stepwise_generation_mode() -> String {
    "auto".to_string()
}

pub fn normalize_stepwise_generation_mode(value: &str) -> String {
    match value.trim() {
        "manual" => "manual".to_string(),
        _ => default_stepwise_generation_mode(),
    }
}

pub fn default_stepwise_max_items() -> u8 {
    4
}

pub fn default_stepwise_max_input_chars() -> u32 {
    6000
}

pub fn default_stepwise_max_output_tokens() -> u32 {
    500
}

pub fn default_stepwise_timeout_ms() -> u64 {
    8000
}

fn default_image_overlay_opacity() -> u8 {
    35
}

fn clamp_image_overlay_opacity(value: u8) -> u8 {
    value.clamp(1, 100)
}

pub fn default_image_overlay_fit_mode() -> String {
    "fit".to_string()
}

fn normalize_image_overlay_fit_mode(value: &str) -> String {
    match value {
        "fill" | "fit" | "stretch" | "tile" | "center" => value.to_string(),
        _ => default_image_overlay_fit_mode(),
    }
}

pub fn clamp_stepwise_max_items(value: u8) -> u8 {
    value.min(6)
}

pub fn clamp_stepwise_max_input_chars(value: u32) -> u32 {
    value.clamp(1000, 24000)
}

pub fn clamp_stepwise_max_output_tokens(value: u32) -> u32 {
    value.clamp(100, 4000)
}

pub fn clamp_stepwise_timeout_ms(value: u64) -> u64 {
    value.clamp(1000, 60000)
}

pub fn default_true() -> bool {
    true
}

pub fn default_relay_base_url() -> String {
    String::new()
}

fn default_weixin_connect_base_url() -> String {
    crate::connect::DEFAULT_WEIXIN_BASE_URL.to_string()
}

fn default_weixin_connect_sandbox() -> String {
    "read-only".to_string()
}

pub fn default_active_relay_id() -> String {
    "default".to_string()
}

pub fn default_relay_test_model() -> String {
    "gpt-5.4-mini".to_string()
}

pub fn default_relay_profiles() -> Vec<RelayProfile> {
    vec![RelayProfile::default()]
}

pub fn default_aggregate_member_weight() -> u32 {
    1
}

pub fn empty_as_default_stepwise_api_key_env<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    Ok(value
        .filter(|value| !value.is_empty())
        .unwrap_or_else(default_stepwise_api_key_env))
}

fn deserialize_stepwise_protocol<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?
        .map(|value| normalize_stepwise_protocol(&value))
        .unwrap_or_else(default_stepwise_protocol))
}

fn deserialize_stepwise_generation_mode<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?
        .map(|value| normalize_stepwise_generation_mode(&value))
        .unwrap_or_else(default_stepwise_generation_mode))
}

fn deserialize_image_overlay_opacity<'de, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<u8>::deserialize(deserializer)?
        .map(clamp_image_overlay_opacity)
        .unwrap_or_else(default_image_overlay_opacity))
}

fn deserialize_image_overlay_fit_mode<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?
        .map(|value| normalize_image_overlay_fit_mode(&value))
        .unwrap_or_else(default_image_overlay_fit_mode))
}

fn deserialize_stepwise_max_items<'de, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<u8>::deserialize(deserializer)?
        .map(clamp_stepwise_max_items)
        .unwrap_or_else(default_stepwise_max_items))
}

fn deserialize_stepwise_max_input_chars<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<u32>::deserialize(deserializer)?
        .map(clamp_stepwise_max_input_chars)
        .unwrap_or_else(default_stepwise_max_input_chars))
}

fn deserialize_stepwise_max_output_tokens<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<u32>::deserialize(deserializer)?
        .map(clamp_stepwise_max_output_tokens)
        .unwrap_or_else(default_stepwise_max_output_tokens))
}

fn deserialize_stepwise_timeout_ms<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<u64>::deserialize(deserializer)?
        .map(clamp_stepwise_timeout_ms)
        .unwrap_or_else(default_stepwise_timeout_ms))
}

fn deserialize_profile_api_key<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

pub fn normalize_codex_extra_args(args: &[String]) -> Vec<String> {
    args.iter()
        .map(|arg| arg.trim())
        .filter(|arg| !arg.is_empty())
        .map(ToString::to_string)
        .collect()
}

#[derive(Debug, Clone)]
pub struct SettingsStore {
    path: PathBuf,
}

impl Default for SettingsStore {
    fn default() -> Self {
        Self::new(crate::paths::default_settings_path())
    }
}

impl SettingsStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn load(&self) -> anyhow::Result<BackendSettings> {
        let contents = match fs::read_to_string(&self.path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut settings = BackendSettings::default();
                settings.sync_tool_shards();
                return Ok(settings);
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read settings {}", self.path.display()));
            }
        };

        let settings = serde_json::from_str(&contents)
            .with_context(|| format!("settings {} 解析失败", self.path.display()))?;
        Ok(normalize_settings_config_sections(settings))
    }

    pub fn save(&self, settings: &BackendSettings) -> anyhow::Result<()> {
        // 现有文件读不懂时，不能用调用方的默认快照覆盖它。
        self.load()?;
        let mut settings = normalize_settings_config_sections(settings.clone());
        settings.codex_extra_args = normalize_codex_extra_args(&settings.codex_extra_args);
        let bytes = serde_json::to_vec_pretty(&settings)?;
        atomic_write(&self.path, &bytes)
    }

    pub fn update(&self, payload: Value) -> anyhow::Result<BackendSettings> {
        let Value::Object(payload) = payload else {
            return self.load();
        };

        let mut raw = self.load_raw_object()?;
        if serde_json::from_value::<BackendSettings>(Value::Object(raw.clone())).is_err() {
            anyhow::bail!("现有 settings 字段类型无效，已拒绝更新");
        }
        if let Some(Value::Array(profiles)) = payload.get("relayProfiles") {
            if serde_json::from_value::<Vec<RelayProfile>>(Value::Array(profiles.clone())).is_err()
            {
                anyhow::bail!("relayProfiles 请求字段无效，已拒绝更新");
            }
        }
        merge_known_setting_fields(&mut raw, &payload);
        let merged = serde_json::from_value(Value::Object(raw.clone()))
            .map_err(|_| anyhow::anyhow!("合并后的 settings 字段类型无效，已拒绝更新"))?;
        let settings = normalize_settings_config_sections(merged);
        raw.insert(
            "relayCommonConfigContents".to_string(),
            Value::String(settings.relay_common_config_contents.clone()),
        );
        raw.insert(
            "relayContextConfigContents".to_string(),
            Value::String(settings.relay_context_config_contents.clone()),
        );
        // 归一化把扁平字段镜像进了 tools.codex，这里写回原始对象，
        // 否则 update 路径保存的分片会停留在迁移前的旧值。
        raw.insert(
            "tools".to_string(),
            serde_json::to_value(&settings.tools).unwrap_or_else(|_| Value::Object(Map::new())),
        );
        let bytes = serde_json::to_vec_pretty(&Value::Object(raw))?;
        atomic_write(&self.path, &bytes)?;
        Ok(settings)
    }

    fn load_raw_object(&self) -> anyhow::Result<Map<String, Value>> {
        let contents = match fs::read_to_string(&self.path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(settings_to_object(&BackendSettings::default()));
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read settings {}", self.path.display()));
            }
        };

        match serde_json::from_str::<Value>(&contents) {
            Ok(Value::Object(map)) => Ok(map),
            Ok(_) => anyhow::bail!("settings {} 顶层不是 JSON 对象", self.path.display()),
            Err(_) => anyhow::bail!("settings {} 解析失败", self.path.display()),
        }
    }
}

fn merge_known_setting_fields(target: &mut Map<String, Value>, source: &Map<String, Value>) {
    target.remove("codexAppPluginAutoExpand");
    target.remove("computerUseGuardEnabled");
    if let Some(value) = source.get("codexAppPath").and_then(Value::as_str) {
        target.insert("codexAppPath".to_string(), Value::String(value.to_string()));
    }
    if let Some(value) = source.get("codexExtraArgs").and_then(Value::as_array) {
        let args = value
            .iter()
            .filter_map(Value::as_str)
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        target.insert(
            "codexExtraArgs".to_string(),
            Value::Array(
                normalize_codex_extra_args(&args)
                    .into_iter()
                    .map(Value::String)
                    .collect(),
            ),
        );
    }
    if let Some(value) = source.get("providerSyncEnabled").and_then(Value::as_bool) {
        target.insert("providerSyncEnabled".to_string(), Value::Bool(value));
    }
    if let Some(value) = source.get("ccsDbPath").and_then(Value::as_str) {
        target.insert(
            "ccsDbPath".to_string(),
            Value::String(value.trim().to_string()),
        );
    }
    if let Some(value) = source.get("relayProfilesEnabled").and_then(Value::as_bool) {
        target.insert("relayProfilesEnabled".to_string(), Value::Bool(value));
    }
    if let Some(value) = source.get("enhancementsEnabled").and_then(Value::as_bool) {
        target.insert("enhancementsEnabled".to_string(), Value::Bool(value));
    }
    merge_bool_setting(target, source, "codexAppPluginMarketplaceUnlock");
    merge_bool_setting(target, source, "codexAppModelWhitelistUnlock");
    merge_bool_setting(target, source, "codexAppSessionDelete");
    merge_bool_setting(target, source, "codexAppMarkdownExport");
    merge_bool_setting(target, source, "codexAppPasteFix");
    merge_bool_setting(target, source, "codexAppForceChineseLocale");
    merge_bool_setting(target, source, "codexAppFastStartup");
    merge_bool_setting(target, source, "codexAppThreadIdBadge");
    merge_bool_setting(target, source, "codexAppConversationView");
    merge_bool_setting(target, source, "codexAppThreadScrollRestore");
    merge_bool_setting(target, source, "codexAppZedRemoteOpen");
    if let Some(value) = source.get("zedRemoteOpenStrategy") {
        if serde_json::from_value::<ZedOpenStrategy>(value.clone()).is_ok() {
            target.insert("zedRemoteOpenStrategy".to_string(), value.clone());
        }
    }
    merge_bool_setting(target, source, "zedRemoteProjectRegistryEnabled");
    merge_bool_setting(target, source, "zedRemoteSyncToZedSettings");
    merge_bool_setting(target, source, "codexAppUpstreamWorktreeCreate");
    merge_bool_setting(target, source, "codexAppNativeMenuPlacement");
    merge_bool_setting(target, source, "codexAppNativeMenuLocalization");
    merge_bool_setting(target, source, "codexAppNativeBrowserRequireIdentification");
    merge_bool_setting(target, source, "codexAppServiceTierControls");
    merge_bool_setting(target, source, "codexAppPetRealMouseLook");
    merge_bool_setting(target, source, "codexAppStepwiseEnabled");
    if let Some(value) = source
        .get("codexAppStepwiseGenerationMode")
        .and_then(Value::as_str)
    {
        target.insert(
            "codexAppStepwiseGenerationMode".to_string(),
            Value::String(normalize_stepwise_generation_mode(value)),
        );
    }
    merge_bool_setting(target, source, "codexAppAnswerOutlineEnabled");
    merge_bool_setting(target, source, "codexAppStepwiseDirectSend");
    if let Some(value) = source
        .get("codexAppStepwiseBaseUrl")
        .and_then(Value::as_str)
    {
        target.insert(
            "codexAppStepwiseBaseUrl".to_string(),
            Value::String(value.trim().trim_end_matches('/').to_string()),
        );
    }
    if let Some(value) = source.get("codexAppStepwiseApiKey").and_then(Value::as_str) {
        target.insert(
            "codexAppStepwiseApiKey".to_string(),
            Value::String(value.trim().to_string()),
        );
    }
    if let Some(value) = source
        .get("codexAppStepwiseApiKeyEnv")
        .and_then(Value::as_str)
    {
        target.insert(
            "codexAppStepwiseApiKeyEnv".to_string(),
            Value::String(if value.trim().is_empty() {
                default_stepwise_api_key_env()
            } else {
                value.trim().to_string()
            }),
        );
    }
    if let Some(value) = source
        .get("codexAppStepwiseProtocol")
        .and_then(Value::as_str)
    {
        target.insert(
            "codexAppStepwiseProtocol".to_string(),
            Value::String(normalize_stepwise_protocol(value)),
        );
    }
    if let Some(value) = source.get("codexAppStepwiseModel").and_then(Value::as_str) {
        target.insert(
            "codexAppStepwiseModel".to_string(),
            Value::String(value.trim().to_string()),
        );
    }
    if let Some(value) = source
        .get("codexAppStepwiseMaxItems")
        .and_then(Value::as_u64)
        .and_then(|value| u8::try_from(value).ok())
    {
        target.insert(
            "codexAppStepwiseMaxItems".to_string(),
            Value::Number(serde_json::Number::from(clamp_stepwise_max_items(value))),
        );
    }
    if let Some(value) = source
        .get("codexAppStepwiseMaxInputChars")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
    {
        target.insert(
            "codexAppStepwiseMaxInputChars".to_string(),
            Value::Number(serde_json::Number::from(clamp_stepwise_max_input_chars(
                value,
            ))),
        );
    }
    if let Some(value) = source
        .get("codexAppStepwiseMaxOutputTokens")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
    {
        target.insert(
            "codexAppStepwiseMaxOutputTokens".to_string(),
            Value::Number(serde_json::Number::from(clamp_stepwise_max_output_tokens(
                value,
            ))),
        );
    }
    if let Some(value) = source
        .get("codexAppStepwiseTimeoutMs")
        .and_then(Value::as_u64)
    {
        target.insert(
            "codexAppStepwiseTimeoutMs".to_string(),
            Value::Number(serde_json::Number::from(clamp_stepwise_timeout_ms(value))),
        );
    }
    merge_bool_setting(target, source, "codexAppImageOverlayEnabled");
    if let Some(value) = source
        .get("codexAppImageOverlayPath")
        .and_then(Value::as_str)
    {
        target.insert(
            "codexAppImageOverlayPath".to_string(),
            Value::String(value.to_string()),
        );
    }
    if let Some(value) = source
        .get("codexAppImageOverlayOpacity")
        .and_then(Value::as_u64)
        .and_then(|value| u8::try_from(value).ok())
    {
        target.insert(
            "codexAppImageOverlayOpacity".to_string(),
            Value::Number(serde_json::Number::from(clamp_image_overlay_opacity(value))),
        );
    }
    if let Some(value) = source
        .get("codexAppImageOverlayFitMode")
        .and_then(Value::as_str)
    {
        target.insert(
            "codexAppImageOverlayFitMode".to_string(),
            Value::String(normalize_image_overlay_fit_mode(value)),
        );
    }
    if let Some(value) = source.get("codexGoalsEnabled").and_then(Value::as_bool) {
        target.insert("codexGoalsEnabled".to_string(), Value::Bool(value));
    }
    merge_bool_setting(target, source, "weixinConnectEnabled");
    for key in [
        "weixinConnectBaseUrl",
        "weixinConnectToken",
        "weixinConnectAccountId",
        "weixinConnectAllowFrom",
        "weixinConnectRouteTag",
        "weixinConnectWorkDir",
        "weixinConnectModel",
        "weixinConnectSandbox",
        "weixinConnectCodexPath",
    ] {
        if let Some(value) = source.get(key).and_then(Value::as_str) {
            target.insert(key.to_string(), Value::String(value.trim().to_string()));
        }
    }
    if let Some(value) = source.get("launchMode").and_then(Value::as_str) {
        if matches!(value, "patch" | "relay") {
            target.insert("launchMode".to_string(), Value::String(value.to_string()));
        }
    }
    if let Some(value) = source.get("relayBaseUrl").and_then(Value::as_str) {
        target.insert("relayBaseUrl".to_string(), Value::String(value.to_string()));
    }
    if let Some(value) = source.get("relayApiKey").and_then(Value::as_str) {
        target.insert("relayApiKey".to_string(), Value::String(value.to_string()));
    }
    if let Some(value) = source.get("relayProfiles").and_then(Value::as_array) {
        let mut profiles = serde_json::from_value::<Vec<RelayProfile>>(Value::Array(value.clone()))
            .unwrap_or_default();
        preserve_official_mix_bearer_tokens(&mut profiles, target);
        target.insert(
            "relayProfiles".to_string(),
            serde_json::to_value(profiles).unwrap_or_else(|_| Value::Array(Vec::new())),
        );
    }
    if let Some(value) = source
        .get("relayCommonConfigContents")
        .and_then(Value::as_str)
    {
        target.insert(
            "relayCommonConfigContents".to_string(),
            Value::String(value.to_string()),
        );
    }
    if let Some(value) = source
        .get("relayContextConfigContents")
        .and_then(Value::as_str)
    {
        target.insert(
            "relayContextConfigContents".to_string(),
            Value::String(value.to_string()),
        );
    }
    if let Some(value) = source.get("activeRelayId").and_then(Value::as_str) {
        target.insert(
            "activeRelayId".to_string(),
            Value::String(value.to_string()),
        );
    }
    if let Some(value) = source
        .get("aggregateRelayProfiles")
        .and_then(Value::as_array)
    {
        target.insert(
            "aggregateRelayProfiles".to_string(),
            Value::Array(value.clone()),
        );
    }
    if let Some(value) = source.get("activeAggregateRelayId").and_then(Value::as_str) {
        target.insert(
            "activeAggregateRelayId".to_string(),
            Value::String(value.to_string()),
        );
    }
    if let Some(value) = source.get("relayTestModel").and_then(Value::as_str) {
        target.insert(
            "relayTestModel".to_string(),
            Value::String(if value.trim().is_empty() {
                default_relay_test_model()
            } else {
                value.trim().to_string()
            }),
        );
    }
    if let Some(value) = source.get("activeTool").and_then(Value::as_str) {
        target.insert(
            "activeTool".to_string(),
            Value::String(ToolId::parse(value).as_str().to_string()),
        );
    }
}

fn merge_bool_setting(target: &mut Map<String, Value>, source: &Map<String, Value>, key: &str) {
    if let Some(value) = source.get(key).and_then(Value::as_bool) {
        target.insert(key.to_string(), Value::Bool(value));
    }
}

fn preserve_official_mix_bearer_tokens(
    profiles: &mut [RelayProfile],
    previous: &Map<String, Value>,
) {
    let previous_tokens = previous
        .get("relayProfiles")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| serde_json::from_value::<RelayProfile>(value.clone()).ok())
        .filter_map(|profile| {
            if profile.relay_mode != RelayMode::Official || !profile.official_mix_api_key {
                return None;
            }
            let token = experimental_bearer_token_from_config_text(&profile.config_contents)?;
            Some((profile.id, token))
        })
        .collect::<HashMap<_, _>>();

    for profile in profiles {
        if profile.relay_mode != RelayMode::Official || !profile.official_mix_api_key {
            continue;
        }
        if experimental_bearer_token_from_config_text(&profile.config_contents).is_some() {
            continue;
        }
        let token = if profile.api_key.trim().is_empty() {
            previous_tokens.get(&profile.id).cloned()
        } else {
            Some(profile.api_key.trim().to_string())
        };
        let Some(token) = token else {
            continue;
        };
        profile.config_contents =
            set_or_replace_experimental_bearer_token(&profile.config_contents, &token);
    }
}

fn set_or_replace_experimental_bearer_token(contents: &str, token: &str) -> String {
    let mut doc = parse_toml_document(contents).unwrap_or_else(|_| DocumentMut::new());
    let session_provider_id =
        active_provider_id(&doc).unwrap_or_else(|| "codex-plus-relay".to_string());
    let transport_provider_id = if session_provider_id == "openai" {
        "custom"
    } else {
        session_provider_id.as_str()
    };
    doc["model_provider"] = toml_edit::value(session_provider_id.as_str());
    doc["model_providers"][transport_provider_id]["experimental_bearer_token"] =
        toml_edit::value(token.trim());
    ensure_text_newline(doc.to_string())
}

fn ensure_text_newline(mut value: String) -> String {
    if !value.is_empty() && !value.ends_with('\n') {
        value.push('\n');
    }
    value
}

fn experimental_bearer_token_from_config_text(contents: &str) -> Option<String> {
    let doc = parse_toml_document(contents).ok()?;
    let provider_id = active_provider_id(&doc)?;
    let token_from = |provider_id: &str| {
        doc.get("model_providers")
            .and_then(Item::as_table)
            .and_then(|providers| providers.get(provider_id))
            .and_then(Item::as_table)
            .and_then(|provider| provider.get("experimental_bearer_token"))
            .and_then(Item::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    };
    token_from(&provider_id).or_else(|| {
        (provider_id == "openai")
            .then(|| token_from("custom"))
            .flatten()
    })
}

fn active_provider_id(doc: &DocumentMut) -> Option<String> {
    doc.get("model_provider")
        .and_then(Item::as_str)
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .map(ToString::to_string)
}

fn parse_toml_document(contents: &str) -> anyhow::Result<DocumentMut> {
    let contents = contents.trim_start_matches('\u{feff}');
    if contents.trim().is_empty() {
        Ok(DocumentMut::new())
    } else {
        contents
            .parse::<DocumentMut>()
            .map_err(|error| anyhow::anyhow!("config.toml TOML 解析失败：{error}"))
    }
}

fn settings_to_object(settings: &BackendSettings) -> Map<String, Value> {
    match serde_json::to_value(settings).unwrap_or_else(|_| Value::Object(Map::new())) {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn normalize_settings_config_sections(mut settings: BackendSettings) -> BackendSettings {
    settings.ccs_db_path = settings.ccs_db_path.trim().to_string();
    let (common, extracted_context) =
        split_context_config_sections(&settings.relay_common_config_contents);
    let context = join_config_sections(&[
        settings.relay_context_config_contents.as_str(),
        extracted_context.as_str(),
    ]);
    settings.relay_common_config_contents = crate::relay_config::normalize_config_text(&common);
    settings.relay_context_config_contents = crate::relay_config::strip_legacy_skill_tables(
        &crate::relay_config::normalize_config_text(&context),
    );
    for profile in &mut settings.relay_profiles {
        let _ = crate::relay_config::normalize_relay_profile_for_storage(profile);
    }
    settings.codex_app_image_overlay_opacity =
        clamp_image_overlay_opacity(settings.codex_app_image_overlay_opacity);
    settings.codex_app_image_overlay_fit_mode =
        normalize_image_overlay_fit_mode(&settings.codex_app_image_overlay_fit_mode);
    settings.codex_app_stepwise_base_url = settings
        .codex_app_stepwise_base_url
        .trim()
        .trim_end_matches('/')
        .to_string();
    settings.codex_app_stepwise_api_key = settings.codex_app_stepwise_api_key.trim().to_string();
    settings.codex_app_stepwise_api_key_env =
        if settings.codex_app_stepwise_api_key_env.trim().is_empty() {
            default_stepwise_api_key_env()
        } else {
            settings.codex_app_stepwise_api_key_env.trim().to_string()
        };
    settings.codex_app_stepwise_protocol =
        normalize_stepwise_protocol(&settings.codex_app_stepwise_protocol);
    settings.codex_app_stepwise_generation_mode =
        normalize_stepwise_generation_mode(&settings.codex_app_stepwise_generation_mode);
    settings.codex_app_stepwise_model = settings.codex_app_stepwise_model.trim().to_string();
    settings.weixin_connect_base_url = settings
        .weixin_connect_base_url
        .trim()
        .trim_end_matches('/')
        .to_string();
    if settings.weixin_connect_base_url.is_empty() {
        settings.weixin_connect_base_url = default_weixin_connect_base_url();
    }
    settings.weixin_connect_token = settings.weixin_connect_token.trim().to_string();
    settings.weixin_connect_account_id = settings.weixin_connect_account_id.trim().to_string();
    settings.weixin_connect_allow_from = settings.weixin_connect_allow_from.trim().to_string();
    settings.weixin_connect_route_tag = settings.weixin_connect_route_tag.trim().to_string();
    settings.weixin_connect_work_dir = settings.weixin_connect_work_dir.trim().to_string();
    settings.weixin_connect_model = settings.weixin_connect_model.trim().to_string();
    settings.weixin_connect_sandbox = match settings.weixin_connect_sandbox.trim() {
        "workspace-write" => "workspace-write",
        "danger-full-access" => "danger-full-access",
        _ => "read-only",
    }
    .to_string();
    settings.weixin_connect_codex_path = settings.weixin_connect_codex_path.trim().to_string();
    settings.codex_app_stepwise_max_items =
        clamp_stepwise_max_items(settings.codex_app_stepwise_max_items);
    settings.codex_app_stepwise_max_input_chars =
        clamp_stepwise_max_input_chars(settings.codex_app_stepwise_max_input_chars);
    settings.codex_app_stepwise_max_output_tokens =
        clamp_stepwise_max_output_tokens(settings.codex_app_stepwise_max_output_tokens);
    settings.codex_app_stepwise_timeout_ms =
        clamp_stepwise_timeout_ms(settings.codex_app_stepwise_timeout_ms);
    // 扁平字段始终是 Codex 的唯一事实来源，这里把它镜像进 tools.codex；
    // 其它工具的分片原样保留。放在函数末尾，所有 load / save / update 路径
    // 都会经过，两边不会漂移。
    settings.sync_tool_shards();
    settings
}

fn split_context_config_sections(config: &str) -> (String, String) {
    let mut common = Vec::new();
    let mut context = Vec::new();
    let mut in_context_table = false;

    for line in config.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_context_table = is_context_table_header(trimmed);
        }
        if in_context_table {
            context.push(line);
        } else {
            common.push(line);
        }
    }

    (
        normalize_text_config(common.join("\n")),
        normalize_text_config(context.join("\n")),
    )
}

fn is_context_table_header(header: &str) -> bool {
    header.starts_with("[mcp_servers.")
        || header.starts_with("[skills.")
        || header.starts_with("[plugins.")
}

fn join_config_sections(sections: &[&str]) -> String {
    let joined = sections
        .iter()
        .map(|section| section.trim())
        .filter(|section| !section.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    normalize_text_config(joined)
}

fn normalize_text_config(contents: String) -> String {
    let trimmed = contents.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    }
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    atomic_write_with(path, |file| file.write_all(bytes))
}

/// 流式原子写入：内容由 `write_contents` 直接写进临时文件，调用方不必先把
/// 完整字节拼在内存里。大文件（如历史会话的 rollout JSONL）走这条路径。
pub fn atomic_write_with(
    path: &Path,
    write_contents: impl FnOnce(&mut File) -> std::io::Result<()>,
) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    // 保留原文件的权限位/只读属性。Windows 上这不包含 DACL；运行时凭据
    // 文件在创建临时文件时另行设置私有 DACL，避免明文写入后的权限修补窗口。
    let existing_permissions = match fs::metadata(path) {
        Ok(metadata) => Some(metadata.permissions()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read permissions for {}", path.display()));
        }
    };
    #[cfg(windows)]
    let private_runtime_document = is_private_runtime_document(path);
    #[cfg(windows)]
    let temp_path = if private_runtime_document {
        private_runtime_temp_path_for(path)
    } else {
        temp_path_for(path)
    };
    #[cfg(not(windows))]
    let temp_path = temp_path_for(path);
    let mut temp_created = false;
    let write_result = (|| -> std::io::Result<()> {
        #[cfg(windows)]
        let mut temp_file = if private_runtime_document {
            create_private_windows_file(&temp_path)?
        } else {
            File::create(&temp_path)?
        };
        #[cfg(not(windows))]
        let mut temp_file = File::create(&temp_path)?;
        temp_created = true;
        write_contents(&mut temp_file)?;
        temp_file.flush()?;
        if let Some(permissions) = existing_permissions {
            temp_file.set_permissions(permissions)?;
        }
        Ok(())
    })();
    if let Err(error) = write_result {
        if temp_created {
            let _ = fs::remove_file(&temp_path);
        }
        return Err(error)
            .with_context(|| format!("failed to write temp file {}", temp_path.display()));
    }
    if let Err(error) = replace_file(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(error).with_context(|| {
            format!(
                "failed to replace {} with {}",
                path.display(),
                temp_path.display()
            )
        });
    }
    Ok(())
}

#[cfg(windows)]
fn is_private_runtime_document(path: &Path) -> bool {
    // 这些确切的 Codex 运行时文件可包含 API Key 或 bearer token。
    // 其他 atomic_write 消费者保持原有文件共享语义。
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.eq_ignore_ascii_case("auth.json") || name.eq_ignore_ascii_case("config.toml")
        })
}

#[cfg(windows)]
fn private_runtime_temp_path_for(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".z8-{}.tmp", uuid::Uuid::new_v4()));
    PathBuf::from(name)
}

#[cfg(windows)]
/// Creates a new directory with a protected DACL; the parent must exist.
pub fn create_private_windows_directory(path: &Path) -> std::io::Result<()> {
    windows_private_fs::create_directory(path)
}

#[cfg(windows)]
/// Creates a new file with a protected DACL and verifies it before returning.
pub fn create_private_windows_file(path: &Path) -> std::io::Result<File> {
    windows_private_fs::create_file(path)
}

/// Verifies that a credential-bearing backup has the same protected DACL as
/// runtime documents before it is treated as private.
#[cfg(windows)]
pub fn verify_private_windows_acl(path: &Path) -> std::io::Result<()> {
    windows_private_fs::verify_private_acl(path)
}

#[cfg(windows)]
pub(crate) mod windows_private_fs {
    use std::ffi::{OsStr, c_void};
    use std::fs::File;
    use std::io;
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use std::path::Path;

    use windows::Win32::Foundation::{BOOL, CloseHandle, HANDLE, HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows::Win32::Security::{
        GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    };
    use windows::Win32::Storage::FileSystem::{
        CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::core::{PCWSTR, PWSTR};

    struct Token(HANDLE);

    impl Drop for Token {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    struct LocalMemory(*mut c_void);

    impl Drop for LocalMemory {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = LocalFree(HLOCAL(self.0));
                }
            }
        }
    }

    fn wide(value: &OsStr) -> Vec<u16> {
        value.encode_wide().chain(std::iter::once(0)).collect()
    }

    fn current_user_sid() -> io::Result<String> {
        let mut handle = HANDLE::default();
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut handle) }
            .map_err(io::Error::other)?;
        let token = Token(handle);
        let mut needed = 0u32;
        let first = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &mut needed) };
        if needed == 0 {
            return Err(io::Error::other(
                first
                    .err()
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| "empty Windows token user".to_string()),
            ));
        }
        let mut words = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
        unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                Some(words.as_mut_ptr().cast()),
                needed,
                &mut needed,
            )
        }
        .map_err(io::Error::other)?;
        let token_user = unsafe { &*(words.as_ptr().cast::<TOKEN_USER>()) };
        let mut sid = PWSTR::null();
        unsafe { ConvertSidToStringSidW(token_user.User.Sid, &mut sid) }
            .map_err(io::Error::other)?;
        let _sid_memory = LocalMemory(sid.0.cast());
        unsafe { sid.to_string() }.map_err(io::Error::other)
    }

    struct PrivateSecurity {
        descriptor: PSECURITY_DESCRIPTOR,
        attributes: SECURITY_ATTRIBUTES,
    }

    impl PrivateSecurity {
        fn new() -> io::Result<Self> {
            let sids = private_policy_sids(current_user_sid()?);
            // Protected DACL: only this token's user, SYSTEM, and local
            // administrators may read the new object. No inherited ACE can
            // grant another local user access before the first byte is written.
            let sddl = format!(
                "D:P{}",
                sids.iter()
                    .map(|sid| format!("(A;;FA;;;{sid})"))
                    .collect::<String>()
            );
            Self::from_sddl(&sddl)
        }

        fn from_sddl(sddl: &str) -> io::Result<Self> {
            let sddl = wide(OsStr::new(sddl));
            let mut descriptor = PSECURITY_DESCRIPTOR::default();
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    PCWSTR(sddl.as_ptr()),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    None,
                )
            }
            .map_err(io::Error::other)?;
            let attributes = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor.0,
                bInheritHandle: BOOL(0),
            };
            Ok(Self { descriptor, attributes })
        }
    }

    impl Drop for PrivateSecurity {
        fn drop(&mut self) {
            unsafe {
                let _ = LocalFree(HLOCAL(self.descriptor.0));
            }
        }
    }

    pub(super) fn create_directory(path: &Path) -> io::Result<()> {
        let security = PrivateSecurity::new()?;
        let wide_path = wide(path.as_os_str());
        unsafe { CreateDirectoryW(PCWSTR(wide_path.as_ptr()), Some(&security.attributes)) }
            .map_err(io::Error::other)?;
        verify_private_acl(path)
    }

    pub(super) fn create_file(path: &Path) -> io::Result<File> {
        let security = PrivateSecurity::new()?;
        let wide_path = wide(path.as_os_str());
        let handle = unsafe {
            CreateFileW(
                PCWSTR(wide_path.as_ptr()),
                0x8000_0000 | 0x4000_0000, // GENERIC_READ | GENERIC_WRITE
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                Some(&security.attributes),
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL,
                HANDLE::default(),
            )
        }
        .map_err(io::Error::other)?;
        let file = unsafe { File::from_raw_handle(handle.0) };
        // Some file systems do not persist ACLs. Re-read the object before
        // the caller writes a credential; fail closed if the protected DACL
        // was ignored or changed.
        verify_private_acl(path)?;
        Ok(file)
    }

    fn private_policy_sids(user_sid: String) -> Vec<String> {
        let mut sids = vec![
            user_sid,
            "S-1-5-18".to_string(),    // SYSTEM
            "S-1-5-32-544".to_string(), // Administrators
        ];
        sids.sort();
        sids.dedup();
        sids
    }

    pub(super) fn verify_private_acl(path: &Path) -> io::Result<()> {
        let (protected, sids) = read_acl(path, true)?;
        if matches_private_policy(protected, sids, &current_user_sid()?) {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Windows file system did not preserve the private runtime DACL",
            ))
        }
    }

    fn matches_private_policy(protected: bool, mut sids: Vec<String>, user_sid: &str) -> bool {
        sids.sort();
        protected && sids == private_policy_sids(user_sid.to_string())
    }

    #[cfg(test)]
    pub(crate) fn test_verify_private_acl(path: &Path) -> io::Result<()> {
        verify_private_acl(path)
    }

    #[cfg(test)]
    pub(crate) fn test_matches_private_policy(
        protected: bool,
        sids: Vec<String>,
        user_sid: &str,
    ) -> bool {
        matches_private_policy(protected, sids, user_sid)
    }

    #[cfg(test)]
    pub(crate) fn make_test_parent_public_read(path: &Path) -> io::Result<()> {
        let sid = current_user_sid()?;
        let sddl = format!(
            "D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;GR;;;AU)"
        );
        set_test_acl(path, &sddl)
    }

    #[cfg(test)]
    pub(crate) fn make_test_file_user_read_only(path: &Path) -> io::Result<()> {
        let sid = current_user_sid()?;
        set_test_acl(path, &format!("D:P(A;;GR;;;{sid})(A;;FA;;;SY)(A;;FA;;;BA)"))
    }

    #[cfg(test)]
    fn set_test_acl(path: &Path, sddl: &str) -> io::Result<()> {
        use windows::Win32::Security::{DACL_SECURITY_INFORMATION, SetFileSecurityW};

        let descriptor = PrivateSecurity::from_sddl(&sddl)?;
        let path = wide(path.as_os_str());
        unsafe {
            SetFileSecurityW(
                PCWSTR(path.as_ptr()),
                DACL_SECURITY_INFORMATION,
                descriptor.descriptor,
            )
        }
        .ok()
        .map_err(io::Error::other)
    }

    #[cfg(test)]
    pub(crate) fn test_acl(path: &Path) -> io::Result<(bool, Vec<String>)> {
        read_acl(path, false)
    }

    fn read_acl(path: &Path, require_exact_access: bool) -> io::Result<(bool, Vec<String>)> {
        use windows::Win32::Security::{
            ACCESS_ALLOWED_ACE, ACL, DACL_SECURITY_INFORMATION, GetAce, GetFileSecurityW,
            GetSecurityDescriptorControl, GetSecurityDescriptorDacl, SE_DACL_PROTECTED,
            SECURITY_DESCRIPTOR_CONTROL,
        };
        use windows::Win32::Storage::FileSystem::FILE_ALL_ACCESS;

        let path = wide(path.as_os_str());
        let mut needed = 0u32;
        unsafe {
            let _ = GetFileSecurityW(
                PCWSTR(path.as_ptr()),
                DACL_SECURITY_INFORMATION.0,
                PSECURITY_DESCRIPTOR::default(),
                0,
                &mut needed,
            );
        }
        if needed == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut words = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
        let descriptor = PSECURITY_DESCRIPTOR(words.as_mut_ptr().cast());
        unsafe {
            GetFileSecurityW(
                PCWSTR(path.as_ptr()),
                DACL_SECURITY_INFORMATION.0,
                descriptor,
                needed,
                &mut needed,
            )
        }
        .ok()
        .map_err(io::Error::other)?;

        let mut control = SECURITY_DESCRIPTOR_CONTROL(0);
        let mut revision = 0u32;
        unsafe { GetSecurityDescriptorControl(descriptor, &mut control.0, &mut revision) }
            .map_err(io::Error::other)?;
        let protected = control.0 & SE_DACL_PROTECTED.0 != 0;
        let mut present = BOOL(0);
        let mut defaulted = BOOL(0);
        let mut dacl: *mut ACL = std::ptr::null_mut();
        unsafe {
            GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted)
        }
        .map_err(io::Error::other)?;
        if present.0 == 0 || dacl.is_null() {
            return Err(io::Error::other("runtime file has no DACL"));
        }

        let mut sids = Vec::new();
        for index in 0..unsafe { (*dacl).AceCount } as u32 {
            let mut ace: *mut c_void = std::ptr::null_mut();
            unsafe { GetAce(dacl, index, &mut ace) }.map_err(io::Error::other)?;
            let allowed = ace.cast::<ACCESS_ALLOWED_ACE>();
            if unsafe { (*allowed).Header.AceType } != 0 {
                return Err(io::Error::other("unexpected ACE type"));
            }
            if require_exact_access
                && unsafe {
                    (*allowed).Mask != FILE_ALL_ACCESS.0 || (*allowed).Header.AceFlags != 0
                }
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "runtime DACL ACE access or flags differ from the private policy",
                ));
            }
            let sid = unsafe {
                windows::Win32::Security::PSID(
                    std::ptr::addr_of!((*allowed).SidStart).cast_mut().cast(),
                )
            };
            let mut text = PWSTR::null();
            unsafe { ConvertSidToStringSidW(sid, &mut text) }.map_err(io::Error::other)?;
            let memory = LocalMemory(text.0.cast());
            sids.push(unsafe { text.to_string() }.map_err(io::Error::other)?);
            drop(memory);
        }
        Ok((protected, sids))
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, target: &Path) -> anyhow::Result<()> {
    fs::rename(source, target)?;
    Ok(())
}

#[cfg(windows)]
fn replace_file(source: &Path, target: &Path) -> anyhow::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    use windows::core::PCWSTR;

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let target = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(target.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )?;
    }
    Ok(())
}

fn temp_path_for(path: &Path) -> PathBuf {
    let mut temp_path = path.to_path_buf();
    let extension = path.extension().and_then(|value| value.to_str());
    temp_path.set_extension(match extension {
        Some(extension) => format!("{extension}.tmp"),
        None => "tmp".to_string(),
    });
    temp_path
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "codex-plus-core-settings-test-{}-{}",
            std::process::id(),
            NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn atomic_write_replaces_existing_file_and_removes_temp_file() {
        let dir = temp_dir();
        let path = dir.join("settings.json");
        std::fs::write(&path, b"old").unwrap();

        atomic_write(&path, b"new").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert!(!dir.join("settings.json.tmp").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn runtime_atomic_write_protects_temp_and_replacement_under_public_parent() {
        let temp = tempfile::tempdir().unwrap();
        windows_private_fs::make_test_parent_public_read(temp.path()).unwrap();
        let (_, parent_sids) = windows_private_fs::test_acl(temp.path()).unwrap();
        assert!(parent_sids.iter().any(|sid| sid == "S-1-5-11"));
        assert!(windows_private_fs::test_verify_private_acl(temp.path()).is_err());

        for name in ["auth.json", "config.toml"] {
            let path = temp.path().join(name);
            std::fs::write(&path, b"old synthetic content").unwrap();
            let (_, old_sids) = windows_private_fs::test_acl(&path).unwrap();
            assert!(old_sids.iter().any(|sid| sid == "S-1-5-11"));

            atomic_write_with(&path, |file| {
                let temps = std::fs::read_dir(temp.path())?
                    .map(|entry| entry.map(|entry| entry.path()))
                    .collect::<std::io::Result<Vec<_>>>()?
                    .into_iter()
                    .filter(|candidate| {
                        candidate
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|file_name| {
                                file_name.starts_with(&format!("{name}.z8-"))
                                    && file_name.ends_with(".tmp")
                            })
                    })
                    .collect::<Vec<_>>();
                assert_eq!(temps.len(), 1);
                let (protected, sids) = windows_private_fs::test_acl(&temps[0])?;
                assert!(protected, "temp DACL must block inheritance before write");
                assert_eq!(sids.len(), 3);
                assert!(!sids.iter().any(|sid| {
                    ["S-1-5-11", "S-1-5-32-545", "S-1-1-0"].contains(&sid.as_str())
                }));
                file.write_all(b"new synthetic content")
            })
            .unwrap();

            assert_eq!(std::fs::read(&path).unwrap(), b"new synthetic content");
            let (protected, sids) = windows_private_fs::test_acl(&path).unwrap();
            assert!(protected, "replacement must retain the private DACL");
            assert_eq!(sids.len(), 3);
            assert!(!sids.iter().any(|sid| {
                ["S-1-5-11", "S-1-5-32-545", "S-1-1-0"].contains(&sid.as_str())
            }));
            assert!(!std::fs::read_dir(temp.path()).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&format!("{name}.z8-"))
            }));
        }

        // Unrelated consumers of atomic_write keep their pre-existing behavior.
        let ordinary = temp.path().join("settings.json");
        atomic_write(&ordinary, b"ordinary synthetic content").unwrap();
        let (_, ordinary_sids) = windows_private_fs::test_acl(&ordinary).unwrap();
        assert!(ordinary_sids.iter().any(|sid| sid == "S-1-5-11"));
    }

    #[cfg(windows)]
    #[test]
    fn runtime_atomic_write_failure_keeps_old_file_and_removes_private_temp() {
        let temp = tempfile::tempdir().unwrap();
        windows_private_fs::make_test_parent_public_read(temp.path()).unwrap();
        let path = temp.path().join("auth.json");
        std::fs::write(&path, b"old synthetic content").unwrap();

        let error = atomic_write_with(&path, |file| {
            file.write_all(b"new synthetic content")?;
            Err(std::io::Error::other("synthetic write failure"))
        })
        .unwrap_err();
        assert!(error.to_string().contains("failed to write temp file"));
        assert_eq!(std::fs::read(&path).unwrap(), b"old synthetic content");
        assert!(std::fs::read_dir(temp.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("auth.json.z8-")
        }));
    }

    #[cfg(windows)]
    #[test]
    fn runtime_acl_readback_rejects_reduced_access_with_expected_sids() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("auth.json");
        drop(create_private_windows_file(&path).unwrap());
        windows_private_fs::make_test_file_user_read_only(&path).unwrap();

        let (protected, sids) = windows_private_fs::test_acl(&path).unwrap();
        assert!(protected);
        assert_eq!(sids.len(), 3);
        assert!(windows_private_fs::test_verify_private_acl(&path).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn runtime_acl_policy_deduplicates_system_user_and_rejects_extra_sid() {
        let system = "S-1-5-18";
        assert!(windows_private_fs::test_matches_private_policy(
            true,
            vec!["S-1-5-32-544".into(), system.into()],
            system,
        ));
        assert!(!windows_private_fs::test_matches_private_policy(
            true,
            vec!["S-1-5-32-544".into(), system.into(), "S-1-5-11".into()],
            system,
        ));
    }

    #[test]
    fn settings_default_matches_expected_behavior() {
        let settings = BackendSettings::default();
        assert!(!settings.provider_sync_enabled);
        assert!(settings.relay_profiles_enabled);
        assert!(settings.enhancements_enabled);
        assert!(settings.codex_app_plugin_marketplace_unlock);
        assert!(!settings.codex_app_thread_id_badge);
        assert!(settings.codex_app_force_chinese_locale);
        assert!(!settings.codex_goals_enabled);
        assert!(settings.codex_app_path.is_empty());
        assert!(settings.codex_extra_args.is_empty());
        assert_eq!(
            settings.zed_remote_open_strategy,
            ZedOpenStrategy::AddToFocusedWorkspace
        );
        assert!(settings.zed_remote_project_registry_enabled);
        assert!(!settings.zed_remote_sync_to_zed_settings);
        assert!(settings.codex_app_native_menu_localization);
        assert_eq!(settings.launch_mode, LaunchMode::Patch);
        assert_eq!(settings.relay_base_url, default_relay_base_url());
        assert!(settings.relay_api_key.is_empty());
        assert_eq!(settings.relay_profiles[0].relay_mode, RelayMode::Official);
        assert!(settings.relay_common_config_contents.is_empty());
        assert_eq!(settings.relay_test_model, default_relay_test_model());
        assert!(!settings.codex_app_stepwise_enabled);
        assert_eq!(settings.codex_app_stepwise_generation_mode, "auto");
        assert!(!settings.codex_app_answer_outline_enabled);
        assert!(!settings.codex_app_stepwise_direct_send);
        assert!(settings.codex_app_stepwise_base_url.is_empty());
        assert!(settings.codex_app_stepwise_api_key.is_empty());
        assert_eq!(
            settings.codex_app_stepwise_api_key_env,
            "CODEX_STEPWISE_API_KEY"
        );
        assert_eq!(settings.codex_app_stepwise_protocol, "chat_completions");
        assert!(settings.codex_app_stepwise_model.is_empty());
        assert_eq!(settings.codex_app_stepwise_max_items, 4);
        assert_eq!(settings.codex_app_stepwise_max_input_chars, 6000);
        assert_eq!(settings.codex_app_stepwise_max_output_tokens, 500);
        assert_eq!(settings.codex_app_stepwise_timeout_ms, 8000);
        assert!(!settings.weixin_connect_enabled);
        assert_eq!(
            settings.weixin_connect_base_url,
            crate::connect::DEFAULT_WEIXIN_BASE_URL
        );
        assert!(settings.weixin_connect_token.is_empty());
        assert_eq!(settings.weixin_connect_sandbox, "read-only");
    }

    #[test]
    fn settings_deserialize_normalizes_stepwise_protocol_and_supports_legacy_missing_field() {
        let defaults: BackendSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(defaults.codex_app_stepwise_protocol, "chat_completions");

        for protocol in [
            "chat_completions",
            "responses",
            "anthropic_messages",
            "auto",
        ] {
            let settings: BackendSettings = serde_json::from_value(json!({
                "codexAppStepwiseProtocol": format!(" {protocol} ")
            }))
            .unwrap();
            assert_eq!(settings.codex_app_stepwise_protocol, protocol);
        }

        let invalid: BackendSettings = serde_json::from_value(json!({
            "codexAppStepwiseProtocol": "unsupported"
        }))
        .unwrap();
        assert_eq!(invalid.codex_app_stepwise_protocol, "chat_completions");
    }

    #[test]
    fn settings_deserialize_defaults_stepwise_ui_settings() {
        let defaults: BackendSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(defaults.codex_app_stepwise_generation_mode, "auto");
        assert!(!defaults.codex_app_answer_outline_enabled);

        let explicitly_enabled: BackendSettings = serde_json::from_value(json!({
            "codexAppAnswerOutlineEnabled": true
        }))
        .unwrap();
        assert!(explicitly_enabled.codex_app_answer_outline_enabled);
    }

    #[test]
    fn settings_deserialize_normalizes_stepwise_generation_mode() {
        let manual: BackendSettings = serde_json::from_value(json!({
            "codexAppStepwiseGenerationMode": " manual "
        }))
        .unwrap();
        assert_eq!(manual.codex_app_stepwise_generation_mode, "manual");

        let invalid: BackendSettings = serde_json::from_value(json!({
            "codexAppStepwiseGenerationMode": "unsupported"
        }))
        .unwrap();
        assert_eq!(invalid.codex_app_stepwise_generation_mode, "auto");
    }

    #[test]
    fn settings_deserialize_ignores_removed_cli_wrapper_keys() {
        let settings: BackendSettings = serde_json::from_str(
            r#"{"codexAppPath":"C:\\Portable\\Codex\\app","providerSyncEnabled":true,"codexGoalsEnabled":true,"cliWrapperEnabled":true,"cliWrapperBaseUrl":"https://example.test","cliWrapperApiKey":"sk-test","cliWrapperApiKeyEnv":""}"#,
        )
        .unwrap();
        assert_eq!(settings.codex_app_path, r"C:\Portable\Codex\app");
        assert!(settings.provider_sync_enabled);
        assert!(settings.codex_goals_enabled);
        assert_eq!(settings.relay_base_url, default_relay_base_url());
        assert!(settings.codex_extra_args.is_empty());
        let saved = serde_json::to_value(&settings).unwrap();
        assert!(saved.get("cliWrapperEnabled").is_none());
        assert!(saved.get("cliWrapperBaseUrl").is_none());
        assert!(saved.get("cliWrapperApiKey").is_none());
        assert!(saved.get("cliWrapperApiKeyEnv").is_none());
    }

    #[test]
    fn settings_deserialize_keeps_plugin_marketplace_unlock_switch() {
        let settings: BackendSettings = serde_json::from_str(
            r#"{
                "codexAppPluginMarketplaceUnlock": true,
                "codexAppPluginAutoExpand": false
            }"#,
        )
        .unwrap();

        assert!(settings.codex_app_plugin_marketplace_unlock);
        let saved = serde_json::to_value(&settings).unwrap();
        assert!(saved.get("codexAppPluginAutoExpand").is_none());

        let legacy_settings: BackendSettings = serde_json::from_str(
            r#"{
                "codexAppForcePluginInstall": false
            }"#,
        )
        .unwrap();

        assert!(legacy_settings.codex_app_plugin_marketplace_unlock);
    }

    #[test]
    fn settings_deserialize_reads_codex_extra_args() {
        let settings: BackendSettings = serde_json::from_str(
            r#"{"codexExtraArgs":["--force_high_performance_gpu"," --ignored-trimmed-by-ui "]}"#,
        )
        .unwrap();

        assert_eq!(
            settings.codex_extra_args,
            vec![
                "--force_high_performance_gpu".to_string(),
                " --ignored-trimmed-by-ui ".to_string(),
            ]
        );
    }

    #[test]
    fn relay_profile_official_mix_api_key_defaults_to_false() {
        let profile: RelayProfile =
            serde_json::from_str(r#"{"id":"official","name":"官方","relayMode":"official"}"#)
                .unwrap();

        assert_eq!(profile.relay_mode, RelayMode::Official);
        assert!(!profile.official_mix_api_key);
        assert!(!profile.hide_official_usage_alert);
        assert!(profile.test_model.is_empty());
    }

    #[test]
    fn relay_profile_context_fields_default_to_empty() {
        let profile = RelayProfile::default();

        assert!(profile.use_common_config);
        assert!(profile.context_window.is_empty());
        assert!(profile.auto_compact_limit.is_empty());
        assert_eq!(profile.model_insert_mode, RelayModelInsertMode::Patch);
        assert!(profile.model_list.is_empty());
        assert!(profile.model_auto_compact.is_empty());
        assert!(profile.model_routes.is_empty());
        assert!(!profile.has_model_routes());
    }

    #[test]
    fn no_auth_relay_requires_protocol_proxy() {
        let settings = BackendSettings {
            relay_profiles: vec![RelayProfile {
                relay_mode: RelayMode::PureApi,
                no_auth: true,
                base_url: "https://relay.example.test/v1".to_string(),
                ..RelayProfile::default()
            }],
            ..BackendSettings::default()
        };

        assert!(settings.active_relay_uses_protocol_proxy());
    }

    #[test]
    fn relay_profile_model_auto_compact_is_opt_in_and_round_trips() {
        let profile = RelayProfile::default();
        let serialized = serde_json::to_value(&profile).unwrap();
        assert!(serialized.get("modelAutoCompact").is_none());

        let profile: RelayProfile = serde_json::from_value(serde_json::json!({
            "id": "relay",
            "name": "Relay",
            "modelAutoCompact": "{\"gpt-5.6-sol\":\"84.329412%\"}"
        }))
        .unwrap();
        assert_eq!(
            profile.model_auto_compact,
            r#"{"gpt-5.6-sol":"84.329412%"}"#
        );
    }

    #[test]
    fn relay_profile_model_metadata_is_opt_in_and_round_trips() {
        let profile = RelayProfile::default();
        let serialized = serde_json::to_value(&profile).unwrap();
        assert!(serialized.get("modelMetadata").is_none());

        let profile: RelayProfile = serde_json::from_value(serde_json::json!({
            "id": "relay",
            "name": "Relay",
            "modelMetadata": "{\"gpt-5.6-sol\":{\"supports_search_tool\":true}}"
        }))
        .unwrap();
        assert_eq!(
            profile.model_metadata,
            r#"{"gpt-5.6-sol":{"supports_search_tool":true}}"#
        );
    }

    #[test]
    fn relay_profile_model_routes_roundtrip_in_camel_case() {
        let profile: RelayProfile = serde_json::from_str(
            r#"{
                "id":"relay-a",
                "name":"供应商 A",
                "modelRoutes":[{
                    "model":"gpt-5.6-luna",
                    "targetRelayId":"relay-b",
                    "targetModel":"provider-luna"
                }]
            }"#,
        )
        .unwrap();

        assert!(profile.has_model_routes());
        assert_eq!(profile.model_routes[0].model, "gpt-5.6-luna");
        assert_eq!(profile.model_routes[0].target_relay_id, "relay-b");
        assert_eq!(profile.model_routes[0].target_model, "provider-luna");

        let saved = serde_json::to_value(profile).unwrap();
        assert_eq!(saved["modelRoutes"][0]["targetRelayId"], "relay-b");
        assert_eq!(saved["modelRoutes"][0]["targetModel"], "provider-luna");
    }

    /// 旧版按供应商勾选上下文条目的 `contextSelection` / `contextSelectionInitialized`
    /// 已被上下文条目自身的 `enabled` 开关取代。历史 settings.json 里仍会带着这两个键，
    /// 反序列化必须容忍它们，否则老用户一升级配置就读不出来。
    #[test]
    fn relay_profile_context_fields_deserialize_from_camel_case() {
        let profile: RelayProfile = serde_json::from_str(
            r#"{
                "id":"relay-a",
                "name":"供应商 A",
                "contextSelection":{
                    "mcpServers":["context7"],
                    "skills":["writer"],
                    "plugins":["local"]
                },
                "contextSelectionInitialized":true,
                "useCommonConfig":false,
                "contextWindow":"200000",
                "autoCompactLimit":"160000",
                "modelInsertMode":"patch",
                "modelList":"qwen3-coder\ndeepseek-coder"
            }"#,
        )
        .unwrap();

        assert!(!profile.use_common_config);
        assert_eq!(profile.context_window, "200000");
        assert_eq!(profile.auto_compact_limit, "160000");
        assert_eq!(profile.model_insert_mode, RelayModelInsertMode::Patch);
        assert_eq!(profile.model_list, "qwen3-coder\ndeepseek-coder");
    }

    #[test]
    fn relay_profile_derived_fields_are_read_but_not_serialized() {
        let profile: RelayProfile = serde_json::from_str(
            r#"{
                "id":"relay-a",
                "name":"供应商 A",
                "model":"gpt-5.4",
                "baseUrl":"https://relay.example/v1",
                "apiKey":"sk-test",
                "configContents":"model = \"gpt-5.4\"\n",
                "authContents":"{\"OPENAI_API_KEY\":\"sk-test\"}"
            }"#,
        )
        .unwrap();

        assert_eq!(profile.model, "gpt-5.4");
        assert_eq!(profile.base_url, "https://relay.example/v1");
        assert_eq!(profile.api_key, "sk-test");

        let saved = serde_json::to_value(&profile).unwrap();
        assert!(saved.get("model").is_none());
        assert!(saved.get("baseUrl").is_none());
        assert!(saved.get("apiKey").is_none());
        assert_eq!(saved["configContents"], "model = \"gpt-5.4\"\n");
        assert_eq!(saved["authContents"], "{\"OPENAI_API_KEY\":\"sk-test\"}");
    }

    #[test]
    fn chat_protocol_profile_roundtrip_migrates_upstream_base_url_out_of_config() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));
        let settings = BackendSettings {
            relay_profiles: vec![RelayProfile {
                id: "relay-chat".to_string(),
                name: "DeepSeek".to_string(),
                protocol: RelayProtocol::ChatCompletions,
                relay_mode: RelayMode::PureApi,
                config_contents: r#"model = "deepseek-chat"
codex_plus_chat_base_url = "https://api.deepseek.com"
model_provider = "custom"

[model_providers.custom]
name = "custom"
wire_api = "responses"
requires_openai_auth = true
base_url = "http://127.0.0.1:57321/v1"
"#
                .to_string(),
                auth_contents: r#"{"OPENAI_API_KEY":"sk-test"}"#.to_string(),
                ..RelayProfile::default()
            }],
            active_relay_id: "relay-chat".to_string(),
            ..BackendSettings::default()
        };

        store.save(&settings).unwrap();
        let loaded = store.load().unwrap();
        let active = loaded.active_relay_profile();

        assert_eq!(active.protocol, RelayProtocol::ChatCompletions);
        assert_eq!(active.base_url, "https://api.deepseek.com");
        assert_eq!(active.upstream_base_url, "https://api.deepseek.com");
        assert_eq!(active.api_key, "sk-test");
        assert!(!active.config_contents.contains("codex_plus_chat_base_url"));

        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("settings.json")).unwrap())
                .unwrap();
        let profile = &saved["relayProfiles"][0];
        assert!(profile.get("baseUrl").is_none());
        assert_eq!(profile["upstreamBaseUrl"], "https://api.deepseek.com");
        assert!(profile.get("apiKey").is_none());
        assert!(
            !profile["configContents"]
                .as_str()
                .unwrap()
                .contains("codex_plus_chat_base_url")
        );
    }

    #[test]
    fn official_profile_without_mix_does_not_persist_api_config() {
        let settings = BackendSettings {
            relay_profiles: vec![RelayProfile {
                id: "official".to_string(),
                name: "官方".to_string(),
                relay_mode: RelayMode::Official,
                official_mix_api_key: false,
                hide_official_usage_alert: false,
                model: "gpt-5.5".to_string(),
                base_url: "https://relay.example/v1".to_string(),
                api_key: "sk-test".to_string(),
                config_contents: r#"model = "gpt-5.5"
model_provider = "custom"

[model_providers.custom]
requires_openai_auth = true
"#
                .to_string(),
                auth_contents: r#"{"OPENAI_API_KEY":"sk-test"}"#.to_string(),
                ..RelayProfile::default()
            }],
            active_relay_id: "official".to_string(),
            ..BackendSettings::default()
        };

        let value = settings_to_object(&normalize_settings_config_sections(settings));
        let profile = &value["relayProfiles"][0];
        assert_eq!(profile["relayMode"], "official");
        assert_eq!(profile["officialMixApiKey"], false);
        assert_eq!(profile["configContents"], "");
        assert_eq!(profile["authContents"], "");
        assert!(profile.get("model").is_none());
        assert!(profile.get("baseUrl").is_none());
        assert!(profile.get("apiKey").is_none());
    }

    #[test]
    fn official_mix_profile_keeps_key_in_config_not_auth() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));
        let settings = BackendSettings {
            relay_profiles: vec![RelayProfile {
                id: "official-mix".to_string(),
                name: "官方混入".to_string(),
                relay_mode: RelayMode::Official,
                official_mix_api_key: true,
                hide_official_usage_alert: false,
                model: "gpt-5.5".to_string(),
                base_url: "https://relay.example/v1".to_string(),
                api_key: "sk-mix".to_string(),
                config_contents: r#"model = "gpt-5.5"
model_provider = "custom"

[model_providers.custom]
requires_openai_auth = true
base_url = "https://relay.example/v1"
experimental_bearer_token = "sk-mix"
"#
                .to_string(),
                auth_contents: r#"{"OPENAI_API_KEY":"sk-mix","auth_mode":"chatgpt"}"#.to_string(),
                ..RelayProfile::default()
            }],
            active_relay_id: "official-mix".to_string(),
            ..BackendSettings::default()
        };

        store.save(&settings).unwrap();
        let loaded = store.load().unwrap();
        let profile = &loaded.relay_profiles[0];

        assert_eq!(profile.relay_mode, RelayMode::Official);
        assert!(profile.official_mix_api_key);
        assert_eq!(profile.api_key, "sk-mix");
        assert!(!profile.auth_contents.contains("OPENAI_API_KEY"));
        assert!(
            profile
                .config_contents
                .contains(r#"experimental_bearer_token = "sk-mix""#)
        );

        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("settings.json")).unwrap())
                .unwrap();
        assert!(saved["relayProfiles"][0].get("apiKey").is_none());
        assert!(
            !saved["relayProfiles"][0]["authContents"]
                .as_str()
                .unwrap()
                .contains("OPENAI_API_KEY")
        );
        assert!(
            saved["relayProfiles"][0]["configContents"]
                .as_str()
                .unwrap()
                .contains(r#"experimental_bearer_token = "sk-mix""#)
        );
    }

    #[test]
    fn settings_update_preserves_official_mix_key_when_payload_loses_it() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));
        store
            .save(&BackendSettings {
                relay_profiles: vec![RelayProfile {
                    id: "official-mix".to_string(),
                    name: "官方混入".to_string(),
                    relay_mode: RelayMode::Official,
                    official_mix_api_key: true,
                    hide_official_usage_alert: false,
                    config_contents: r#"model_provider = "custom"

[model_providers.other]
base_url = "https://other.example/v1"
experimental_bearer_token = "sk-other"

[model_providers.custom]
base_url = "https://relay.example/v1"
experimental_bearer_token = "sk-existing"
"#
                    .to_string(),
                    ..RelayProfile::default()
                }],
                active_relay_id: "official-mix".to_string(),
                ..BackendSettings::default()
            })
            .unwrap();

        let updated = store
            .update(json!({
                "relayProfiles": [{
                    "id": "official-mix",
                    "name": "官方混入",
                    "relayMode": "official",
                    "officialMixApiKey": true,
                    "configContents": "model_provider = \"custom\"\n\n[model_providers.other]\nbase_url = \"https://other.example/v1\"\nexperimental_bearer_token = \"sk-other\"\n\n[model_providers.custom]\nbase_url = \"https://relay.example/v1\"\nexperimental_bearer_token = \"\"\n",
                    "authContents": ""
                }],
                "activeRelayId": "official-mix"
            }))
            .unwrap();

        let profile = &updated.relay_profiles[0];
        assert_eq!(profile.api_key, "sk-existing");
        assert!(!profile.config_contents.contains("sk-other"));
        assert!(profile.config_contents.contains(
            r#"[model_providers.custom]
base_url = "https://relay.example/v1"
experimental_bearer_token = "sk-existing""#
        ));
    }

    #[test]
    fn official_mix_update_uses_api_key_when_config_token_missing() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        let updated = store
            .update(json!({
                "relayProfiles": [{
                    "id": "official-mix",
                    "name": "官方混入",
                    "relayMode": "official",
                    "officialMixApiKey": true,
                    "baseUrl": "https://relay.example/v1",
                    "apiKey": "sk-new",
                    "configContents": "model_provider = \"custom\"\n\n[model_providers.custom]\nbase_url = \"https://relay.example/v1\"\n",
                    "authContents": ""
                }],
                "activeRelayId": "official-mix"
            }))
            .unwrap();

        let profile = &updated.relay_profiles[0];
        assert_eq!(profile.api_key, "sk-new");
        assert!(
            profile
                .config_contents
                .contains(r#"experimental_bearer_token = "sk-new""#)
        );
        assert!(!profile.auth_contents.contains("OPENAI_API_KEY"));
    }

    #[test]
    fn settings_update_preserves_manual_official_mix_config_token() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        let updated = store
            .update(json!({
                "relayProfiles": [{
                    "id": "official-mix",
                    "name": "官方混入",
                    "relayMode": "official",
                    "officialMixApiKey": true,
                    "configContents": "model_provider = \"custom\"\n\n[model_providers.custom]\nbase_url = \"https://relay.example/v1\"\nexperimental_bearer_token = \"22222222222222222222222222222222222\"\n",
                    "authContents": ""
                }],
                "activeRelayId": "official-mix"
            }))
            .unwrap();

        let profile = &updated.relay_profiles[0];
        assert_eq!(profile.relay_mode, RelayMode::Official);
        assert!(profile.official_mix_api_key);
        assert_eq!(profile.api_key, "22222222222222222222222222222222222");
        assert!(
            profile
                .config_contents
                .contains(r#"experimental_bearer_token = "22222222222222222222222222222222222""#)
        );
        assert!(!profile.auth_contents.contains("OPENAI_API_KEY"));
    }

    fn normalized_default_settings() -> BackendSettings {
        // Keep expected values independent of the production normalizer.
        let mut expected = BackendSettings::default();
        expected.tools.insert(ToolId::Codex, ToolConfig::default());
        expected
    }

    #[test]
    fn settings_store_load_missing_file_returns_default() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        assert_eq!(store.load().unwrap(), normalized_default_settings());
    }

    #[test]
    fn settings_store_load_bad_json_returns_error_and_preserves_file() {
        let dir = temp_dir();
        let path = dir.join("settings.json");
        std::fs::write(&path, "{bad json").unwrap();
        let store = SettingsStore::new(path.clone());

        assert!(store.load().is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "{bad json");
    }

    #[test]
    fn settings_store_update_bad_json_returns_error_and_preserves_file() {
        let dir = temp_dir();
        let path = dir.join("settings.json");
        std::fs::write(&path, "{bad json").unwrap();
        let store = SettingsStore::new(path.clone());

        assert!(store.update(json!({ "providerSyncEnabled": true })).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "{bad json");
    }

    #[test]
    fn settings_store_update_rejects_invalid_profile_without_dropping_existing_profiles() {
        let dir = temp_dir();
        let path = dir.join("settings.json");
        let existing = r#"{"relayProfiles":[{"id":"one","name":"One"},{"id":"two","name":"Two"}]}"#;
        std::fs::write(&path, existing).unwrap();
        let store = SettingsStore::new(path.clone());

        assert!(store
            .update(json!({ "relayProfiles": [
                { "id": "one", "name": "One" },
                { "id": 42, "name": "invalid" }
            ] }))
            .is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), existing);
    }

    #[test]
    fn settings_store_save_rejects_unreadable_existing_settings() {
        for existing in ["{bad json", r#"{"relayProfiles":"wrong-type"}"#] {
            let dir = temp_dir();
            let path = dir.join("settings.json");
            std::fs::write(&path, existing).unwrap();
            let store = SettingsStore::new(path.clone());

            assert!(store.save(&BackendSettings::default()).is_err());
            assert_eq!(std::fs::read_to_string(path).unwrap(), existing);
        }
    }

    #[test]
    fn settings_store_save_still_allows_explicit_reset_of_valid_settings() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));
        store
            .update(json!({ "relayProfiles": [
                { "id": "one", "name": "One" },
                { "id": "two", "name": "Two" }
            ] }))
            .unwrap();

        store.save(&BackendSettings::default()).unwrap();
        assert_eq!(store.load().unwrap().relay_profiles, default_relay_profiles());
    }

    #[test]
    fn settings_store_save_load_roundtrip_uses_custom_path() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("nested").join("settings.json"));
        let settings = BackendSettings {
            provider_sync_enabled: true,
            codex_extra_args: vec!["--force_high_performance_gpu".to_string()],
            ccs_db_path: dir.join("cc-switch.db").to_string_lossy().to_string(),
            ..BackendSettings::default()
        };

        store.save(&settings).unwrap();

        let mut expected = settings;
        expected.tools.insert(ToolId::Codex, ToolConfig::default());
        assert_eq!(store.load().unwrap(), expected);
    }

    #[test]
    fn settings_store_model_routes_restore_target_credentials() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));
        let profile = |id: &str, base_url: &str, api_key: &str| RelayProfile {
            id: id.to_string(),
            name: id.to_string(),
            relay_mode: RelayMode::PureApi,
            upstream_base_url: base_url.to_string(),
            config_contents: format!(
                "model_provider = \"custom\"\n\n[model_providers.custom]\nname = \"custom\"\nwire_api = \"responses\"\nbase_url = \"{base_url}\"\n"
            ),
            auth_contents: format!(r#"{{"OPENAI_API_KEY":"{api_key}"}}"#),
            ..RelayProfile::default()
        };
        let mut source = profile("source", "https://source.example/v1", "sk-source");
        source.model_routes = vec![RelayModelRoute {
            model: "gpt-5.6-luna".to_string(),
            target_relay_id: "target".to_string(),
            target_model: String::new(),
        }];
        let settings = BackendSettings {
            active_relay_id: "source".to_string(),
            relay_profiles: vec![
                source,
                profile("target", "https://target.example/v1", "sk-target"),
            ],
            ..BackendSettings::default()
        };

        store.save(&settings).unwrap();
        let loaded = store.load().unwrap();

        assert!(loaded.active_relay_uses_protocol_proxy());
        assert_eq!(
            loaded.relay_profiles[0].base_url,
            "https://source.example/v1"
        );
        assert_eq!(
            loaded.relay_profiles[1].base_url,
            "https://target.example/v1"
        );
        assert_eq!(loaded.relay_profiles[1].api_key, "sk-target");
        assert_eq!(
            loaded.relay_profiles[0].model_routes[0].target_relay_id,
            "target"
        );
    }

    #[test]
    fn settings_store_persists_and_normalizes_stepwise_protocol_and_generation_mode() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        let updated = store
            .update(json!({
                "codexAppStepwiseProtocol": "responses",
                "codexAppStepwiseGenerationMode": " manual "
            }))
            .unwrap();
        assert_eq!(updated.codex_app_stepwise_protocol, "responses");
        assert_eq!(
            store.load().unwrap().codex_app_stepwise_protocol,
            "responses"
        );
        assert_eq!(updated.codex_app_stepwise_generation_mode, "manual");
        assert_eq!(
            store.load().unwrap().codex_app_stepwise_generation_mode,
            "manual"
        );

        let invalid = store
            .update(json!({
                "codexAppStepwiseProtocol": "not-a-protocol",
                "codexAppStepwiseGenerationMode": "not-a-mode"
            }))
            .unwrap();
        assert_eq!(invalid.codex_app_stepwise_protocol, "chat_completions");
        assert_eq!(invalid.codex_app_stepwise_generation_mode, "auto");
        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(store.path).unwrap()).unwrap();
        assert_eq!(saved["codexAppStepwiseProtocol"], "chat_completions");
        assert_eq!(saved["codexAppStepwiseGenerationMode"], "auto");
    }

    #[test]
    fn settings_store_save_load_roundtrip_preserves_aggregate_relay_settings() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));
        let settings = BackendSettings {
            relay_profiles: vec![
                RelayProfile {
                    id: "relay-a".to_string(),
                    name: "中转 A".to_string(),
                    ..RelayProfile::default()
                },
                RelayProfile {
                    id: "relay-b".to_string(),
                    name: "中转 B".to_string(),
                    ..RelayProfile::default()
                },
                RelayProfile {
                    id: "agg".to_string(),
                    name: "聚合".to_string(),
                    relay_mode: RelayMode::Aggregate,
                    ..RelayProfile::default()
                },
            ],
            active_relay_id: "agg".to_string(),
            aggregate_relay_profiles: vec![AggregateRelayProfile {
                id: "agg".to_string(),
                name: "聚合".to_string(),
                session_provider: RelaySessionProvider::Openai,
                strategy: AggregateRelayStrategy::WeightedRoundRobin,
                members: vec![
                    AggregateRelayMember {
                        relay_id: "relay-a".to_string(),
                        weight: 1,
                    },
                    AggregateRelayMember {
                        relay_id: "relay-b".to_string(),
                        weight: 3,
                    },
                ],
                routes: Vec::new(),
            }],
            active_aggregate_relay_id: "agg".to_string(),
            ..BackendSettings::default()
        };

        store.save(&settings).unwrap();

        let loaded = store.load().unwrap();
        let expected = normalize_settings_config_sections(settings);
        let active_aggregate = loaded.active_aggregate_relay_profile().unwrap();
        assert_eq!(loaded, expected);
        assert_eq!(
            active_aggregate.strategy,
            AggregateRelayStrategy::WeightedRoundRobin
        );
        assert_eq!(active_aggregate.members[1].relay_id, "relay-b");
        assert_eq!(active_aggregate.members[1].weight, 3);
        assert_eq!(
            active_aggregate.session_provider,
            RelaySessionProvider::Openai
        );
        assert_eq!(
            loaded.active_relay_session_provider(),
            RelaySessionProvider::Openai
        );
        assert!(loaded.active_relay_uses_protocol_proxy());
    }

    #[test]
    fn active_relay_session_provider_reads_standard_profile_config() {
        let mut settings = BackendSettings {
            relay_profiles: vec![RelayProfile {
                config_contents: "model_provider = \"openai\"\n".to_string(),
                ..RelayProfile::default()
            }],
            ..BackendSettings::default()
        };

        assert_eq!(
            settings.active_relay_session_provider(),
            RelaySessionProvider::Openai
        );

        settings.relay_profiles[0].config_contents = "model_provider = \"custom\"\n".to_string();
        assert_eq!(
            settings.active_relay_session_provider(),
            RelaySessionProvider::Custom
        );
    }

    #[test]
    fn settings_store_update_only_mutates_present_known_fields() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));
        let initial = BackendSettings {
            provider_sync_enabled: false,
            ..BackendSettings::default()
        };
        store.save(&initial).unwrap();

        let updated = store
            .update(json!({
            "providerSyncEnabled": true,
            "codexAppPath": "C:\\Portable\\Codex\\Codex.exe",
            "enhancementsEnabled": false,
            "codexAppSessionDelete": false,
            "codexAppConversationView": true,
            "codexAppThreadIdBadge": true,
            "codexAppNativeMenuLocalization": false,
            "codexAppServiceTierControls": true,
            "codexAppPetRealMouseLook": true,
            "codexGoalsEnabled": true,
            "relayBaseUrl": "https://relay.example.test/v1",
            "relayApiKey": "sk-relay",
            "codexExtraArgs": ["--force_high_performance_gpu", "", "  ", " --enable-gpu "],
            "unknownKey": "ignored"
            }))
            .unwrap();

        assert!(updated.provider_sync_enabled);
        assert_eq!(updated.codex_app_path, r"C:\Portable\Codex\Codex.exe");
        assert!(!updated.enhancements_enabled);
        assert!(!updated.codex_app_session_delete);
        assert!(updated.codex_app_conversation_view);
        assert!(updated.codex_app_thread_id_badge);
        assert!(!updated.codex_app_native_menu_localization);
        assert!(updated.codex_app_service_tier_controls);
        assert!(updated.codex_app_pet_real_mouse_look);
        assert!(updated.codex_goals_enabled);
        assert_eq!(updated.relay_base_url, "https://relay.example.test/v1");
        assert_eq!(updated.relay_api_key, "sk-relay");
        assert_eq!(
            updated.codex_extra_args,
            vec![
                "--force_high_performance_gpu".to_string(),
                "--enable-gpu".to_string(),
            ]
        );
        assert_eq!(store.load().unwrap(), updated);
    }

    #[test]
    fn settings_store_update_persists_image_overlay_settings() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        let updated = store
            .update(json!({
                "codexAppImageOverlayEnabled": true,
                "codexAppImageOverlayPath": "C:\\Users\\me\\Pictures\\overlay.png",
                "codexAppImageOverlayOpacity": 42,
                "codexAppImageOverlayFitMode": "fill"
            }))
            .unwrap();

        assert!(updated.codex_app_image_overlay_enabled);
        assert_eq!(
            updated.codex_app_image_overlay_path,
            r"C:\Users\me\Pictures\overlay.png"
        );
        assert_eq!(updated.codex_app_image_overlay_opacity, 42);
        assert_eq!(updated.codex_app_image_overlay_fit_mode, "fill");
        assert_eq!(store.load().unwrap(), updated);
    }

    #[test]
    fn settings_store_defaults_invalid_image_overlay_fit_mode_to_fit() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        let updated = store
            .update(json!({
                "codexAppImageOverlayFitMode": "unknown"
            }))
            .unwrap();

        assert_eq!(updated.codex_app_image_overlay_fit_mode, "fit");
    }

    #[test]
    fn settings_store_update_persists_stepwise_settings() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        let updated = store
            .update(json!({
                "codexAppStepwiseEnabled": true,
                "codexAppStepwiseGenerationMode": "manual",
                "codexAppAnswerOutlineEnabled": false,
                "codexAppStepwiseDirectSend": true,
                "codexAppStepwiseBaseUrl": "https://api.example.test/v1/",
                "codexAppStepwiseApiKey": " sk-stepwise ",
                "codexAppStepwiseApiKeyEnv": "",
                "codexAppStepwiseModel": " stepwise-mini ",
                "codexAppStepwiseMaxItems": 12,
                "codexAppStepwiseMaxInputChars": 25000,
                "codexAppStepwiseMaxOutputTokens": 50,
                "codexAppStepwiseTimeoutMs": 70000
            }))
            .unwrap();

        assert!(updated.codex_app_stepwise_enabled);
        assert_eq!(updated.codex_app_stepwise_generation_mode, "manual");
        assert!(!updated.codex_app_answer_outline_enabled);
        assert!(updated.codex_app_stepwise_direct_send);
        assert_eq!(
            updated.codex_app_stepwise_base_url,
            "https://api.example.test/v1"
        );
        assert_eq!(updated.codex_app_stepwise_api_key, "sk-stepwise");
        assert_eq!(
            updated.codex_app_stepwise_api_key_env,
            default_stepwise_api_key_env()
        );
        assert_eq!(updated.codex_app_stepwise_model, "stepwise-mini");
        assert_eq!(updated.codex_app_stepwise_max_items, 6);
        assert_eq!(updated.codex_app_stepwise_max_input_chars, 24000);
        assert_eq!(updated.codex_app_stepwise_max_output_tokens, 100);
        assert_eq!(updated.codex_app_stepwise_timeout_ms, 60000);
        assert_eq!(store.load().unwrap(), updated);
    }

    #[test]
    fn settings_store_update_persists_weixin_connect_settings() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        let updated = store
            .update(json!({
                "weixinConnectEnabled": true,
                "weixinConnectBaseUrl": "https://ilink.example.test/",
                "weixinConnectToken": " token ",
                "weixinConnectAccountId": " bot-1 ",
                "weixinConnectAllowFrom": " user@im.wechat ",
                "weixinConnectRouteTag": " route ",
                "weixinConnectWorkDir": " /workspace ",
                "weixinConnectModel": " gpt-test ",
                "weixinConnectSandbox": "workspace-write",
                "weixinConnectCodexPath": " /usr/local/bin/codex "
            }))
            .unwrap();

        assert!(updated.weixin_connect_enabled);
        assert_eq!(
            updated.weixin_connect_base_url,
            "https://ilink.example.test"
        );
        assert_eq!(updated.weixin_connect_token, "token");
        assert_eq!(updated.weixin_connect_account_id, "bot-1");
        assert_eq!(updated.weixin_connect_allow_from, "user@im.wechat");
        assert_eq!(updated.weixin_connect_route_tag, "route");
        assert_eq!(updated.weixin_connect_work_dir, "/workspace");
        assert_eq!(updated.weixin_connect_model, "gpt-test");
        assert_eq!(updated.weixin_connect_sandbox, "workspace-write");
        assert_eq!(updated.weixin_connect_codex_path, "/usr/local/bin/codex");
        assert_eq!(store.load().unwrap(), updated);
    }

    #[test]
    fn settings_store_update_persists_launch_mode() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        let updated = store.update(json!({"launchMode": "relay"})).unwrap();
        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("settings.json")).unwrap())
                .unwrap();

        assert_eq!(updated.launch_mode, LaunchMode::Relay);
        assert_eq!(saved["launchMode"], json!("relay"));
    }

    #[test]
    fn settings_store_update_persists_relay_profiles_and_active_profile() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        let updated = store
            .update(json!({
                "relayProfiles": [
                    {
                        "id": "relay-a",
                        "name": "中转 A",
                        "baseUrl": "https://relay-a.example/v1",
                        "apiKey": "sk-a"
                    },
                    {
                        "id": "relay-b",
                        "name": "中转 B",
                        "baseUrl": "https://relay-b.example/v1",
                        "apiKey": "sk-b"
                    }
                ],
                "activeRelayId": "relay-b",
                "relayTestModel": "claude-sonnet-4"
            }))
            .unwrap();

        let active = updated.active_relay_profile();
        assert_eq!(updated.relay_profiles.len(), 2);
        assert_eq!(active.id, "relay-b");
        assert_eq!(active.name, "中转 B");
        assert_eq!(updated.relay_test_model, "claude-sonnet-4");

        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("settings.json")).unwrap())
                .unwrap();
        assert!(saved["relayProfiles"][1].get("baseUrl").is_none());
        assert!(saved["relayProfiles"][1].get("apiKey").is_none());
    }

    #[test]
    fn settings_store_update_does_not_persist_relay_profile_derived_fields() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        let updated = store
            .update(json!({
                "relayProfiles": [
                    {
                        "id": "relay-a",
                        "name": "供应商 A",
                        "model": "gpt-5.4",
                        "baseUrl": "https://relay.example/v1",
                        "apiKey": "sk-a",
                        "configContents": "model = \"gpt-5.4\"\n",
                        "authContents": "{\"OPENAI_API_KEY\":\"sk-a\"}"
                    }
                ],
                "activeRelayId": "relay-a"
            }))
            .unwrap();

        assert_eq!(updated.relay_profiles[0].id, "relay-a");
        assert_eq!(updated.relay_profiles[0].name, "供应商 A");

        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("settings.json")).unwrap())
                .unwrap();
        let saved_profile = &saved["relayProfiles"][0];
        assert!(saved_profile.get("model").is_none());
        assert!(saved_profile.get("baseUrl").is_none());
        assert!(saved_profile.get("apiKey").is_none());
        assert_eq!(saved_profile["configContents"], "model = \"gpt-5.4\"\n");
        assert_eq!(
            saved_profile["authContents"],
            "{\"OPENAI_API_KEY\":\"sk-a\"}"
        );
    }

    #[test]
    fn settings_store_update_moves_context_tables_out_of_common_config() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        let updated = store
            .update(json!({
                "relayCommonConfigContents": "[mcp_servers.context7]\ncommand = \"npx\"\n"
            }))
            .unwrap();

        assert!(updated.relay_common_config_contents.is_empty());
        assert_eq!(
            updated.relay_context_config_contents,
            "[mcp_servers.context7]\ncommand = \"npx\"\n"
        );
        assert_eq!(store.load().unwrap(), updated);
    }

    #[test]
    fn settings_store_update_extracts_context_config_from_common_config() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        let updated = store
            .update(json!({
                "relayCommonConfigContents": "model_reasoning_effort = \"high\"\n\n[mcp_servers.context7]\ncommand = \"npx\"\n\n[plugins.\"superpowers@openai-curated\"]\nenabled = true\n"
            }))
            .unwrap();

        assert_eq!(
            updated.relay_common_config_contents,
            "model_reasoning_effort = \"high\"\n"
        );
        assert!(
            updated
                .relay_context_config_contents
                .contains("[mcp_servers.context7]")
        );
        assert!(
            updated
                .relay_context_config_contents
                .contains("[plugins.\"superpowers@openai-curated\"]")
        );
        assert_eq!(store.load().unwrap(), updated);
    }

    #[test]
    fn settings_store_update_persists_aggregate_relay_profiles_and_active_id() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        let updated = store
            .update(json!({
                "relayProfiles": [
                    { "id": "relay-a", "name": "中转 A" },
                    { "id": "relay-b", "name": "中转 B" },
                    { "id": "agg", "name": "聚合", "relayMode": "aggregate" }
                ],
                "activeRelayId": "agg",
                "aggregateRelayProfiles": [
                    {
                        "id": "agg",
                        "name": "聚合",
                        "strategy": "weightedRoundRobin",
                        "members": [
                            { "relayId": "relay-a", "weight": 1 },
                            { "relayId": "relay-b", "weight": 4 }
                        ]
                    }
                ],
                "activeAggregateRelayId": "agg"
            }))
            .unwrap();

        let active_aggregate = updated.active_aggregate_relay_profile().unwrap();
        assert_eq!(updated.active_relay_id, "agg");
        assert_eq!(updated.active_aggregate_relay_id, "agg");
        assert_eq!(
            active_aggregate.strategy,
            AggregateRelayStrategy::WeightedRoundRobin
        );
        assert_eq!(active_aggregate.members.len(), 2);
        assert_eq!(active_aggregate.members[1].relay_id, "relay-b");
        assert_eq!(active_aggregate.members[1].weight, 4);
        assert!(updated.active_relay_uses_protocol_proxy());
    }

    #[test]
    fn active_relay_profile_uses_legacy_single_relay_when_profiles_are_default() {
        let settings = BackendSettings {
            relay_base_url: "https://legacy.example/v1".to_string(),
            relay_api_key: "sk-legacy".to_string(),
            ..BackendSettings::default()
        };

        let active = settings.active_relay_profile();

        assert_eq!(active.id, "default");
        assert_eq!(active.name, "默认中转");
        assert_eq!(active.base_url, "https://legacy.example/v1");
        assert_eq!(active.api_key, "sk-legacy");
        assert_eq!(active.relay_mode, RelayMode::MixedApi);
        assert!(active.official_mix_api_key);
    }

    #[test]
    fn settings_store_update_preserves_existing_unknown_fields() {
        let dir = temp_dir();
        let path = dir.join("settings.json");
        let store = SettingsStore::new(path.clone());
        std::fs::write(
            &path,
            r#"{"providerSyncEnabled":false,"customField":{"nested":true}}"#,
        )
        .unwrap();

        let updated = store
            .update(json!({
                "providerSyncEnabled": true
            }))
            .unwrap();
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

        assert!(updated.provider_sync_enabled);
        assert_eq!(saved["providerSyncEnabled"], json!(true));
        assert_eq!(saved["codexExtraArgs"], Value::Null);
        assert_eq!(saved["customField"], json!({"nested": true}));
    }

    #[test]
    fn settings_store_update_removes_obsolete_setting_fields() {
        let dir = temp_dir();
        let path = dir.join("settings.json");
        let store = SettingsStore::new(path.clone());
        std::fs::write(
            &path,
            r#"{"providerSyncEnabled":false,"codexAppPluginAutoExpand":true,"computerUseGuardEnabled":true,"customField":1}"#,
        )
        .unwrap();

        store
            .update(json!({
                "providerSyncEnabled": true
            }))
            .unwrap();
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

        assert!(saved.get("codexAppPluginAutoExpand").is_none());
        assert!(saved.get("computerUseGuardEnabled").is_none());
        assert_eq!(saved["customField"], json!(1));
    }

    #[test]
    fn settings_store_update_persists_codex_extra_args_and_preserves_unknown_fields() {
        let dir = temp_dir();
        let path = dir.join("settings.json");
        let store = SettingsStore::new(path.clone());
        std::fs::write(
            &path,
            r#"{"providerSyncEnabled":false,"customField":{"nested":true}}"#,
        )
        .unwrap();

        let updated = store
            .update(json!({
                "codexExtraArgs": ["--force_high_performance_gpu", "--enable-features=UseOzonePlatform"]
            }))
            .unwrap();
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

        assert_eq!(
            updated.codex_extra_args,
            vec![
                "--force_high_performance_gpu".to_string(),
                "--enable-features=UseOzonePlatform".to_string(),
            ]
        );
        assert_eq!(
            saved["codexExtraArgs"],
            json!([
                "--force_high_performance_gpu",
                "--enable-features=UseOzonePlatform"
            ])
        );
        assert_eq!(saved["customField"], json!({"nested": true}));
    }

    #[test]
    fn settings_store_update_with_non_object_payload_does_not_write_file() {
        let dir = temp_dir();
        let path = dir.join("settings.json");
        let store = SettingsStore::new(path.clone());
        let original = r#"{"providerSyncEnabled":false,"customField":"keep me"}"#;
        std::fs::write(&path, original).unwrap();

        let updated = store.update(json!(null)).unwrap();

        assert!(!updated.provider_sync_enabled);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn relay_profile_standard_openai_protocol_defaults_off_for_legacy_profiles() {
        // 旧 profile 没有该字段：反序列化后默认关闭。
        let mut enabled = RelayProfile::default();
        enabled.standard_openai_protocol = true;
        let mut legacy = serde_json::to_value(&enabled).unwrap();
        legacy
            .as_object_mut()
            .unwrap()
            .remove("standardOpenaiProtocol");
        let profile: RelayProfile = serde_json::from_value(legacy).unwrap();
        assert!(!profile.standard_openai_protocol);
    }

    #[test]
    fn relay_profile_standard_openai_protocol_round_trip_keeps_existing_providers() {
        // 关闭时导出不写该字段，round-trip 不改变既有 provider。
        let value = serde_json::to_value(RelayProfile::default()).unwrap();
        assert!(value.get("standardOpenaiProtocol").is_none());

        let mut enabled = RelayProfile::default();
        enabled.standard_openai_protocol = true;
        let value = serde_json::to_value(&enabled).unwrap();
        assert_eq!(value["standardOpenaiProtocol"], json!(true));
        let round_tripped: RelayProfile = serde_json::from_value(value).unwrap();
        assert!(round_tripped.standard_openai_protocol);
    }
}
