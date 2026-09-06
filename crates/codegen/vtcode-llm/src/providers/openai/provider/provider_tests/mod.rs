//! OpenAI provider unit tests split by provider behaviour.

use super::super::CustomProviderAuthHandle;
use super::super::backend_setup::{
    CHATGPT_CODEX_BASE, ChatGptSubscriptionAuthSource, OpenAIBackendKind, OpenAIBackendRefreshBehaviour,
    OpenAIBackendSetup,
};
use super::super::tool_serialization;
use super::*;
use crate::provider::{LLMProvider, NormalizedStreamEvent, ParallelToolConfig};
use futures::StreamExt;
use reqwest::StatusCode;
use rig::providers::chatgpt::ChatGPTAuth as RigChatGptAuth;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;
use vtcode_config::TimeoutsConfig;
use vtcode_config::auth::{AuthCredentialsStoreMode, OpenAIChatGptAuthHandle, OpenAIChatGptSession};
use vtcode_config::constants::urls;
use vtcode_config::core::CustomProviderCommandAuthConfig;
use vtcode_config::core::{
    CustomProviderApiFormat, OpenAIHostedShellConfig, OpenAIHostedShellDomainSecret, OpenAIHostedShellEnvironment,
    OpenAIHostedShellNetworkPolicy, OpenAIHostedShellNetworkPolicyType, OpenAIHostedSkill, OpenAIHostedSkillVersion,
    OpenAIServiceTier, PromptCacheRetention,
};
use vtcode_config::{OpenAIAuthConfig, auth::OpenAIChatGptSessionRefresher};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ─── Test Fixtures ───────────────────────────────────────────────────────────

mod schema_support;
mod support;

use support::*;

mod backend_auth;
mod cache_config;
mod chat_payloads;
mod chatgpt_history;
mod format_routing;
mod provider_capabilities;
mod request_execution;
mod responses_core;
mod responses_tools;
mod stream_requests;
mod tool_payloads;
