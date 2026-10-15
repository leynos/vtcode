//! Applies client-selected configuration changes to an active ACP session.
use super::super::ZedAgent;
use crate::acp;
use crate::zed::types::SessionHandle;
use vtcode_core::config::types::ReasoningEffortLevel;

impl ZedAgent {
    /// Programmatic equivalent of the SACP `session/set_config_option`
    /// handler. Used both by the SACP handler and by the test suite.
    pub(crate) async fn set_session_config_option(
        &self,
        args: acp::SetSessionConfigOptionRequest,
    ) -> Result<acp::SetSessionConfigOptionResponse, acp::Error> {
        use crate::zed::helpers::SESSION_CONFIG_MODEL_ID;
        use crate::zed::helpers::SESSION_CONFIG_PRIMARY_AGENT_ID;
        use crate::zed::helpers::SESSION_CONFIG_PROVIDER_ID;
        use crate::zed::helpers::SESSION_CONFIG_THOUGHT_LEVEL_ID;

        let Some(session) = self
            .session_handle(&args.session_id)
            .map_err(|error| acp::Error::internal_error().data(error.to_string()))?
        else {
            return Err(acp::Error::invalid_params().data(serde_json::json!({
                "reason": "unknown_session"
            })));
        };

        let config_id = args.config_id.0.to_string();
        let value = match &args.value {
            acp::SessionConfigOptionValue::ValueId { value } => value.0.as_ref().to_string(),
            _ => {
                return Err(acp::Error::invalid_params().data(serde_json::json!({
                    "reason": "expected_string_config_option_value",
                    "config_id": config_id,
                })));
            }
        };
        let updated = match config_id.as_str() {
            SESSION_CONFIG_PRIMARY_AGENT_ID => self.set_primary_agent_option(&session, &args.session_id, &value)?,
            SESSION_CONFIG_THOUGHT_LEVEL_ID => self.set_thought_level_option(&session, &config_id, &value)?,
            SESSION_CONFIG_PROVIDER_ID => self.set_provider_option(&session, &value)?,
            SESSION_CONFIG_MODEL_ID => self.set_model_option(&session, &value)?,
            _ => {
                return Err(acp::Error::invalid_params().data(serde_json::json!({
                    "reason": "unknown_config_option",
                    "config_id": config_id,
                })));
            }
        };

        self.send_config_option_update_if_changed(&session, &args.session_id, updated)
            .await?;

        let config_options = self
            .current_session_config_options(&session)
            .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
        Ok(acp::SetSessionConfigOptionResponse::new(config_options))
    }

    fn set_primary_agent_option(
        &self,
        session: &SessionHandle,
        session_id: &acp::SessionId,
        value: &str,
    ) -> Result<bool, acp::Error> {
        let workspace_runtime = session.workspace_runtime().map_err(|error| {
            acp::Error::internal_error()
                .data(format!("Failed to read workspace runtime for ACP session {session_id}: {error:#}"))
        })?;
        let primary_agents = workspace_runtime
            .as_ref()
            .map_or(&self.primary_agents, |runtime| &runtime.primary_agents);
        let Some(primary_agent) = primary_agents.resolve_id(value) else {
            return Err(acp::Error::invalid_params().data(serde_json::json!({
                "reason": "unknown_primary_agent",
                "value": value,
            })));
        };
        self.update_session_primary_agent(session, primary_agent.to_string())
            .map_err(|error| acp::Error::internal_error().data(error.to_string()))
    }

    fn set_thought_level_option(
        &self,
        session: &SessionHandle,
        config_id: &str,
        value: &str,
    ) -> Result<bool, acp::Error> {
        let (provider, model) = {
            let data = session.data.lock().map_err(|error| {
                acp::Error::internal_error().data(format!("Failed to read ACP session config: {error}"))
            })?;
            (data.provider.clone(), data.model.clone())
        };
        if !self.model_supports_thought_level(&provider, &model) {
            return Err(acp::Error::invalid_params().data(serde_json::json!({
                "reason": "unsupported_config_option",
                "config_id": config_id,
            })));
        }
        let Some(reasoning_effort) = ReasoningEffortLevel::parse(value) else {
            return Err(acp::Error::invalid_params().data(serde_json::json!({
                "reason": "unknown_thought_level",
                "value": value,
            })));
        };
        self.update_session_reasoning_effort(session, reasoning_effort)
            .map_err(|error| acp::Error::internal_error().data(error.to_string()))
    }

    fn set_provider_option(&self, session: &SessionHandle, value: &str) -> Result<bool, acp::Error> {
        let provider = value.trim().to_lowercase();
        let current_provider = {
            let data = session.data.lock().map_err(|error| {
                acp::Error::internal_error().data(format!("Failed to read ACP session config: {error}"))
            })?;
            data.provider.clone()
        };
        if provider.is_empty() || !self.supports_provider(&provider, &current_provider) {
            return Err(acp::Error::invalid_params().data(serde_json::json!({
                "reason": "unknown_provider",
                "value": value,
            })));
        }
        let current_model = {
            let data = session.data.lock().map_err(|error| {
                acp::Error::internal_error().data(format!("Failed to read ACP session config: {error}"))
            })?;
            data.model.clone()
        };
        let resolved_model = if self.provider_supports_model(&provider, &current_model) {
            current_model
        } else {
            self.provider_default_model(&provider).ok_or_else(|| {
                acp::Error::invalid_params().data(serde_json::json!({
                    "reason": "provider_has_no_default_model",
                    "provider": provider,
                }))
            })?
        };
        self.update_session_provider_and_model(session, provider, resolved_model)
            .map_err(|error| acp::Error::internal_error().data(error.to_string()))
    }

    fn set_model_option(&self, session: &SessionHandle, value: &str) -> Result<bool, acp::Error> {
        let model = value.trim();
        if model.is_empty() {
            return Err(acp::Error::invalid_params().data(serde_json::json!({
                "reason": "unknown_model",
                "value": value,
            })));
        }
        let provider = {
            let data = session.data.lock().map_err(|error| {
                acp::Error::internal_error().data(format!("Failed to read ACP session config: {error}"))
            })?;
            data.provider.clone()
        };
        if !self.provider_supports_model(&provider, model) {
            return Err(acp::Error::invalid_params().data(serde_json::json!({
                "reason": "model_not_supported_for_provider",
                "provider": provider,
                "model": model,
            })));
        }
        self.update_session_provider_and_model(session, provider, model.to_string())
            .map_err(|error| acp::Error::internal_error().data(error.to_string()))
    }

    async fn send_config_option_update_if_changed(
        &self,
        session: &SessionHandle,
        session_id: &acp::SessionId,
        updated: bool,
    ) -> Result<(), acp::Error> {
        if updated {
            let config_options = self
                .current_session_config_options(session)
                .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
            let update = acp::ConfigOptionUpdate::new(config_options);
            drop(
                self.send_update(session_id, acp::SessionUpdate::ConfigOptionUpdate(update))
                    .await,
            );
        }
        Ok(())
    }
}
