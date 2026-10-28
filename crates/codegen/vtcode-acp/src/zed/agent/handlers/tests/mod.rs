//! Shared fixtures and protocol harness for ACP prompt, provider, session, and tool tests.
use super::super::ZedAgentSetup;
use super::prompt::*;
use super::provider::*;
use super::*;

mod archive_test_support;
mod client;
mod prompt;
mod provider;
mod session;
mod tools;

use agent_client_protocol::{Channel, on_receive_notification};
use assert_fs::TempDir;
use async_trait::async_trait;
use proptest::prelude::*;
use std::collections::BTreeMap;
use std::fmt::Write as FmtWrite;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};
use vtcode_config::SubagentDiscoveryInput;
use vtcode_config::auth::AuthCredentialsStoreMode;
use vtcode_config::core::{CustomProviderConfig, CustomProviderRequestPolicyConfig};
use vtcode_core::config::core::PromptCachingConfig;
use vtcode_core::config::types::{
    AgentConfig as CoreAgentConfig, ModelSelectionSource, ReasoningEffortLevel, UiSurfacePreference,
};
use vtcode_core::config::{AgentClientProtocolZedConfig, CommandsConfig, ToolsConfig, VTCodeConfig};
use vtcode_core::core::agent::snapshots::{DEFAULT_CHECKPOINTS_ENABLED, DEFAULT_MAX_AGE_DAYS, DEFAULT_MAX_SNAPSHOTS};
use vtcode_core::core::threads::{ThreadBootstrap, ThreadManager};

mod prompt_fixtures;
mod provider_runtime_fixtures;

use archive_test_support::{EnvGuard, SESSION_DIR_ENV, lock_history_test_guard};
use prompt_fixtures::*;
use provider_runtime_fixtures::*;
