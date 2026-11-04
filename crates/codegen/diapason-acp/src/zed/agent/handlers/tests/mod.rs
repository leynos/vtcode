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
use diapason_config::SubagentDiscoveryInput;
use diapason_config::auth::AuthCredentialsStoreMode;
use diapason_config::core::{CustomProviderConfig, CustomProviderRequestPolicyConfig};
use diapason_core::config::core::PromptCachingConfig;
use diapason_core::config::types::{
    AgentConfig as CoreAgentConfig, ModelSelectionSource, ReasoningEffortLevel, UiSurfacePreference,
};
use diapason_core::config::{AgentClientProtocolZedConfig, CommandsConfig, DiapasonConfig, ToolsConfig};
use diapason_core::core::agent::snapshots::{DEFAULT_CHECKPOINTS_ENABLED, DEFAULT_MAX_AGE_DAYS, DEFAULT_MAX_SNAPSHOTS};
use diapason_core::core::threads::{ThreadBootstrap, ThreadManager};
use proptest::prelude::*;
use std::collections::BTreeMap;
use std::fmt::Write as FmtWrite;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

mod prompt_fixtures;
mod provider_runtime_fixtures;

use archive_test_support::{EnvGuard, SESSION_DIR_ENV, lock_history_test_guard};
use prompt_fixtures::*;
use provider_runtime_fixtures::*;
