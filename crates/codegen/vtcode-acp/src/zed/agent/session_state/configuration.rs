//! ACP session configuration projection and updates.

mod update;

use super::super::ZedAgent;
use crate::acp;
use crate::zed::helpers::{PrimaryAgentCatalog, session_config_options};
use crate::zed::types::{SessionHandle, lock_poison_error};
use anyhow::Context;
use std::str::FromStr;
use vtcode_core::config::models::{ModelId, Provider};
use vtcode_core::config::types::ReasoningEffortLevel;
use vtcode_core::llm::ModelResolver;
use vtcode_core::llm::factory::get_factory;

impl ZedAgent {
    pub(super) fn session_reasoning_effort_for_thread(
        &self,
        thread: &vtcode_core::core::threads::ThreadRuntimeHandle,
    ) -> ReasoningEffortLevel {
        thread
            .metadata()
            .and_then(|metadata| ReasoningEffortLevel::parse(&metadata.reasoning_effort))
            .unwrap_or(self.config.reasoning_effort)
    }

    fn sync_thread_reasoning_effort(
        &self,
        thread: &vtcode_core::core::threads::ThreadRuntimeHandle,
        reasoning_effort: ReasoningEffortLevel,
    ) {
        if let Some(mut metadata) = thread.metadata() {
            metadata.reasoning_effort = reasoning_effort.as_str().to_string();
            thread.replace_metadata(Some(metadata));
        }
    }
    pub(super) fn session_provider_for_thread(
        &self,
        thread: &vtcode_core::core::threads::ThreadRuntimeHandle,
    ) -> String {
        thread
            .metadata()
            .map(|metadata| metadata.provider)
            .unwrap_or_else(|| self.config.provider.clone())
    }

    pub(super) fn session_model_for_thread(&self, thread: &vtcode_core::core::threads::ThreadRuntimeHandle) -> String {
        thread
            .metadata()
            .map(|metadata| metadata.model)
            .unwrap_or_else(|| self.config.model.clone())
    }

    pub(super) fn session_primary_agent_for_thread(
        &self,
        thread: &vtcode_core::core::threads::ThreadRuntimeHandle,
        primary_agents: &PrimaryAgentCatalog,
    ) -> String {
        thread
            .metadata()
            .and_then(|metadata| metadata.primary_agent)
            .and_then(|primary_agent| primary_agents.resolve_id(&primary_agent))
            .map(ToString::to_string)
            .unwrap_or_else(|| primary_agents.default_id().to_string())
    }

    fn sync_thread_primary_agent(&self, thread: &vtcode_core::core::threads::ThreadRuntimeHandle, primary_agent: &str) {
        if let Some(mut metadata) = thread.metadata() {
            metadata.primary_agent = Some(primary_agent.to_string());
            thread.replace_metadata(Some(metadata));
        }
    }

    fn sync_thread_provider_and_model(
        &self,
        thread: &vtcode_core::core::threads::ThreadRuntimeHandle,
        provider: &str,
        model: &str,
    ) {
        if let Some(mut metadata) = thread.metadata() {
            metadata.provider = provider.to_string();
            metadata.model = model.to_string();
            thread.replace_metadata(Some(metadata));
        }
    }

    fn model_supports_thought_level(&self, provider: &str, model: &str) -> bool {
        ModelResolver::resolve(Some(provider), model, &[], None)
            .map(|resolved| resolved.reasoning_supported())
            .unwrap_or(false)
    }

    pub(in crate::zed::agent) fn update_session_primary_agent(
        &self,
        session: &SessionHandle,
        primary_agent: String,
    ) -> anyhow::Result<bool> {
        let workspace_runtime = session
            .workspace_runtime()
            .context("reading ACP session workspace runtime to update primary agent")?;
        let primary_agents = workspace_runtime
            .as_ref()
            .map_or(&self.primary_agents, |runtime| &runtime.primary_agents);
        let Some(primary_agent) = primary_agents.resolve_id(&primary_agent) else {
            return Ok(false);
        };
        let mut data = session
            .data
            .lock()
            .map_err(|error| lock_poison_error("updating ACP session primary agent", error))?;
        if data.primary_agent.eq_ignore_ascii_case(primary_agent) {
            return Ok(false);
        }
        data.primary_agent = primary_agent.to_string();
        self.sync_thread_primary_agent(&data.thread, &data.primary_agent);
        Ok(true)
    }

    pub(super) fn update_session_reasoning_effort(
        &self,
        session: &SessionHandle,
        reasoning_effort: ReasoningEffortLevel,
    ) -> anyhow::Result<bool> {
        let mut data = session
            .data
            .lock()
            .map_err(|error| lock_poison_error("updating ACP session reasoning effort", error))?;
        if data.reasoning_effort == reasoning_effort {
            return Ok(false);
        }
        data.reasoning_effort = reasoning_effort;
        self.sync_thread_reasoning_effort(&data.thread, reasoning_effort);
        Ok(true)
    }

    pub(super) fn update_session_provider_and_model(
        &self,
        session: &SessionHandle,
        provider: String,
        model: String,
    ) -> anyhow::Result<bool> {
        let mut data = session
            .data
            .lock()
            .map_err(|error| lock_poison_error("updating ACP session provider and model", error))?;
        if data.provider == provider && data.model == model {
            return Ok(false);
        }
        data.provider = provider;
        data.model = model;
        self.sync_thread_provider_and_model(&data.thread, &data.provider, &data.model);
        Ok(true)
    }

    fn provider_default_model(&self, provider: &str) -> Option<String> {
        Provider::from_str(provider)
            .ok()
            .map(|value| ModelId::default_single_for_provider(value).as_str().to_string())
    }

    fn provider_supports_model(&self, provider: &str, model: &str) -> bool {
        let Ok(provider) = Provider::from_str(provider) else {
            return true;
        };
        ModelId::models_for_provider(provider)
            .iter()
            .any(|entry| entry.as_str() == model)
    }

    fn provider_select_options(&self, current_provider: &str) -> Vec<acp::SessionConfigSelectOption> {
        let mut providers = get_factory()
            .lock()
            .ok()
            .map(|factory| factory.list_providers())
            .unwrap_or_default();
        if providers.is_empty() {
            tracing::warn!("LLM factory has no registered providers, falling back to Provider::all_providers()");
            providers = Provider::all_providers()
                .into_iter()
                .map(|provider| provider.to_string())
                .collect();
        }

        if !providers.iter().any(|provider| provider.eq_ignore_ascii_case(current_provider)) {
            providers.push(current_provider.to_string());
        }

        providers.sort();
        providers.dedup_by(|left, right| left.eq_ignore_ascii_case(right));

        tracing::debug!(
            provider_count = providers.len(),
            providers = ?providers,
            current_provider = current_provider,
            "Building provider select options for ACP"
        );

        providers
            .into_iter()
            .map(|provider| {
                let name = Provider::from_str(&provider)
                    .ok()
                    .map(|parsed| parsed.label().to_string())
                    .unwrap_or_else(|| provider.clone());
                acp::SessionConfigSelectOption::new(provider, name)
            })
            .collect()
    }

    fn model_select_options(&self, provider: &str, current_model: &str) -> Vec<acp::SessionConfigSelectOption> {
        let mut options = Provider::from_str(provider)
            .ok()
            .map(|provider| {
                ModelId::models_for_provider(provider)
                    .into_iter()
                    .map(|model| {
                        acp::SessionConfigSelectOption::new(
                            model.as_str().into_owned(),
                            model.display_name().into_owned(),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        if !options.iter().any(|option| option.value.0.as_ref() == current_model) {
            options.push(acp::SessionConfigSelectOption::new(current_model.to_string(), current_model.to_string()));
        }

        options.sort_by(|left, right| left.value.0.cmp(&right.value.0));
        options
    }

    fn supports_provider(&self, provider: &str, current_provider: &str) -> bool {
        self.provider_select_options(current_provider)
            .iter()
            .any(|option| option.value.0.as_ref() == provider)
    }

    pub(super) fn current_session_config_options(
        &self,
        session: &SessionHandle,
    ) -> anyhow::Result<Vec<acp::SessionConfigOption>> {
        let data = session
            .data
            .lock()
            .map_err(|error| lock_poison_error("building ACP session config options", error))?;
        let provider_options = self.provider_select_options(&data.provider);
        let model_options = self.model_select_options(&data.provider, &data.model);
        let primary_agents = data
            .workspace_runtime
            .as_ref()
            .map_or(&self.primary_agents, |runtime| &runtime.primary_agents);
        let config_options = session_config_options(
            &data.primary_agent,
            primary_agents,
            data.reasoning_effort,
            self.model_supports_thought_level(&data.provider, &data.model),
            &data.provider,
            provider_options,
            &data.model,
            model_options,
        );

        tracing::debug!(
            config_option_count = config_options.len(),
            primary_agent = %data.primary_agent,
            current_provider = %data.provider,
            current_model = %data.model,
            "Built session config options for ACP"
        );

        Ok(config_options)
    }
}
