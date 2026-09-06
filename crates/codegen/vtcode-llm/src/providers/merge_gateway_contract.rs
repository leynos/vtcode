//! Merge Gateway wire/domain contract boundary.
//!
//! Focused serde-friendly models for catalogue and Responses payloads.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use vtcode_commons::tool_types::CompactStr;

fn default_false() -> bool {
    false
}

fn default_list_object() -> CompactStr {
    "list".into()
}

fn default_response_object() -> CompactStr {
    "response".into()
}

fn default_usd() -> CompactStr {
    "USD".into()
}

fn default_standard_service_tiers() -> Vec<MergeServiceTier> {
    vec![MergeServiceTier::Standard]
}

fn is_false(value: &bool) -> bool {
    !*value
}

macro_rules! string_enum {
    ($name:ident { $($variant:ident => $value:literal,)+ }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($variant,)+
            #[serde(other)]
            Unknown,
        }
    };
}

string_enum! {
    MergeAvailabilityStatus {
        Available => "available",
        Deprecated => "deprecated",
    }
}

string_enum! {
    MergeInputModality {
        Text => "text",
        Image => "image",
        Document => "document",
        Embedding => "embedding",
    }
}

string_enum! {
    MergeOutputModality {
        Text => "text",
        ToolUse => "tool_use",
        Embedding => "embedding",
    }
}

string_enum! {
    MergeServiceTier {
        Standard => "standard",
        Flex => "flex",
        Priority => "priority",
    }
}

string_enum! {
    MergeMessageRole {
        User => "user",
        Assistant => "assistant",
        System => "system",
        Developer => "developer",
        Tool => "tool",
    }
}

string_enum! {
    MergeToolChoiceMode {
        Auto => "auto",
        None => "none",
        Required => "required",
    }
}

string_enum! {
    MergeFinishReason {
        Stop => "stop",
        Length => "length",
        ToolUse => "tool_use",
        ContentFilter => "content_filter",
        Error => "error",
    }
}

string_enum! {
    MergeResponseFormatType {
        JsonObject => "json_object",
        JsonSchema => "json_schema",
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MergeModelsListQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<CompactStr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<CompactStr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<CompactStr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<CompactStr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

impl MergeModelsListQuery {
    pub fn by_cursor(cursor: impl Into<CompactStr>) -> Self {
        Self { cursor: Some(cursor.into()), ..Self::default() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeModelCatalogueResponse {
    #[serde(default = "default_list_object")]
    pub object: CompactStr,
    pub data: Vec<MergeModelRecord>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub has_more: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<CompactStr>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

impl MergeModelCatalogueResponse {
    pub fn validate_envelope(&self) -> Result<()> {
        ensure!(self.object == "list", "expected Merge model catalogue envelope");
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeModelRecord {
    pub availability_status: MergeAvailabilityStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<CompactStr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<CompactStr>,
    pub model: CompactStr,
    pub provider: CompactStr,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<CompactStr>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vendors: BTreeMap<CompactStr, MergeVendorModelInfo>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeVendorModelInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_date: Option<CompactStr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability_status: Option<MergeAvailabilityStatus>,
    pub capabilities: MergeVendorCapabilities,
    pub pricing: MergeModelPricing,
    #[serde(default = "default_standard_service_tiers", skip_serializing_if = "Vec::is_empty")]
    pub service_tiers: Vec<MergeServiceTier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zdr: Option<bool>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeVendorCapabilities {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input: Vec<MergeInputModality>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output: Vec<MergeOutputModality>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub supports_tool_calling: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub supports_tool_choice: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub supports_structured_outputs: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub streaming: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<MergeReasoningCapability>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

impl MergeVendorCapabilities {
    pub fn supports_reasoning(&self) -> bool {
        self.reasoning.is_some()
    }

    pub fn reasoning_controls(&self) -> Vec<CompactStr> {
        self.reasoning
            .as_ref()
            .map(|reasoning| reasoning.controls.clone())
            .unwrap_or_default()
    }

    pub fn reasoning_disable_supported(&self) -> bool {
        self.reasoning
            .as_ref()
            .map(|reasoning| reasoning.disable_supported)
            .unwrap_or(false)
    }
}

/// Per-vendor reasoning capability advertised through Merge's `/v1/models`
/// catalogue. Reasoning controls are route-specific: either a provider-native
/// `reasoning_effort` or a Gateway-managed `thinking.budget_tokens`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeReasoningCapability {
    #[serde(default, skip_serializing_if = "is_false")]
    pub configurable: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub disable_supported: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub controls: Vec<CompactStr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_style: Option<CompactStr>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeModelPricing {
    #[serde(default = "default_usd", skip_serializing_if = "is_default_usd")]
    pub currency: CompactStr,
    pub input_per_million: f64,
    pub output_per_million: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flex: Option<MergeTierPricing>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<MergeTierPricing>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

fn is_default_usd(value: &CompactStr) -> bool {
    value.as_str() == "USD"
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeTierPricing {
    pub input_per_million: f64,
    pub output_per_million: f64,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeToolDefinition {
    #[serde(rename = "type")]
    pub kind: CompactStr,
    pub name: CompactStr,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<CompactStr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

impl MergeToolDefinition {
    pub fn function(name: impl Into<CompactStr>, description: impl Into<CompactStr>, parameters: Value) -> Self {
        Self {
            kind: "function".into(),
            name: name.into(),
            description: Some(description.into()),
            parameters: Some(parameters),
            strict: None,
            extra: Map::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeToolChoiceFunction {
    pub name: CompactStr,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeSpecificToolChoice {
    #[serde(rename = "type")]
    pub kind: CompactStr,
    pub function: MergeToolChoiceFunction,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MergeToolChoice {
    Mode(MergeToolChoiceMode),
    Specific(MergeSpecificToolChoice),
    Raw(Value),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeJsonSchemaResponseFormat {
    pub name: CompactStr,
    pub schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MergeResponseFormat {
    JsonObject {
        #[serde(rename = "type")]
        kind: MergeResponseFormatType,
    },
    JsonSchema {
        #[serde(rename = "type")]
        kind: MergeResponseFormatType,
        #[serde(rename = "json_schema")]
        json_schema: MergeJsonSchemaResponseFormat,
    },
    Raw(Value),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MergeResponsesInputContent {
    Text(CompactStr),
    Parts(Vec<MergeResponsesInputContentPart>),
    Raw(Value),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum MergeResponsesInputContentPart {
    #[serde(rename = "text")]
    Text {
        text: CompactStr,
        #[serde(flatten, default)]
        extra: Map<String, Value>,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: CompactStr,
        content: Value,
        #[serde(flatten, default)]
        extra: Map<String, Value>,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: CompactStr,
        name: CompactStr,
        input: Value,
        #[serde(flatten, default)]
        extra: Map<String, Value>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeResponsesMessageInput {
    #[serde(rename = "type")]
    pub kind: CompactStr,
    pub role: MergeMessageRole,
    pub content: MergeResponsesInputContent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<CompactStr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<CompactStr>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MergeResponsesInputItem {
    Message(MergeResponsesMessageInput),
    Raw(Value),
}

impl MergeResponsesInputItem {
    pub fn message(role: MergeMessageRole, content: MergeResponsesInputContent) -> Self {
        Self::Message(MergeResponsesMessageInput {
            kind: "message".into(),
            role,
            content,
            id: None,
            status: None,
            extra: Map::new(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MergeResponsesRequest {
    pub input: Vec<MergeResponsesInputItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<CompactStr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer: Option<CompactStr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_policy_id: Option<CompactStr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<MergeToolDefinition>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<MergeToolChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<CompactStr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<MergeResponseFormat>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub stream: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<MergeTag>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<CompactStr>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub include_routing_metadata: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<CompactStr>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vendors: Vec<CompactStr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<MergeServiceTier>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub service_tier_fallback: bool,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

impl MergeResponsesRequest {
    pub fn new(input: Vec<MergeResponsesInputItem>) -> Self {
        Self { input, ..Self::default() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeTag {
    pub key: CompactStr,
    pub value: CompactStr,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeResponseUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cost: f64,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeResponseRoutingMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<CompactStr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_request_id: Option<CompactStr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<CompactStr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routed_model: Option<CompactStr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<MergeServiceTier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MergeResponseContentPart {
    Text(MergeResponseTextContent),
    ToolUse(MergeResponseToolUseContent),
    Refusal(MergeResponseRefusalContent),
    Raw(Value),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeResponseTextContent {
    #[serde(rename = "type")]
    pub kind: CompactStr,
    pub text: CompactStr,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Vec<Value>>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeResponseToolUseContent {
    #[serde(rename = "type")]
    pub kind: CompactStr,
    pub id: CompactStr,
    pub name: CompactStr,
    pub input: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u64>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeResponseRefusalContent {
    #[serde(rename = "type")]
    pub kind: CompactStr,
    pub refusal: CompactStr,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeResponseMessageOutput {
    #[serde(rename = "type")]
    pub kind: CompactStr,
    pub id: CompactStr,
    pub role: MergeMessageRole,
    pub content: Vec<MergeResponseContentPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<MergeFinishReason>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MergeResponseOutputItem {
    Message(MergeResponseMessageOutput),
    Raw(Value),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeResponsesResponse {
    pub id: CompactStr,
    #[serde(default = "default_response_object")]
    pub object: CompactStr,
    pub created_at: CompactStr,
    pub model: CompactStr,
    pub output: Vec<MergeResponseOutputItem>,
    pub usage: MergeResponseUsage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<CompactStr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_request_id: Option<CompactStr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<MergeResponseRoutingMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<MergeServiceTier>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

impl MergeResponsesResponse {
    pub fn validate_envelope(&self) -> Result<()> {
        ensure!(self.object == "response", "expected Merge responses envelope");
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MergeResponsesSseData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<CompactStr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_text: Option<CompactStr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_item: Option<MergeResponseOutputItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<MergeResponseUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<CompactStr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_request_id: Option<CompactStr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<MergeResponseRoutingMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<MergeServiceTier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<MergeResponsesResponse>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MergeResponsesSseEvent {
    pub event: CompactStr,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<CompactStr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<MergeResponsesSseData>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

impl MergeResponsesSseEvent {
    pub fn new(event: impl Into<CompactStr>) -> Self {
        Self {
            event: event.into(),
            id: None,
            data: None,
            extra: Map::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_models_catalogue_decodes_pagination_and_capabilities() {
        let payload = json!({
            "object": "list",
            "data": [
                {
                    "availability_status": "available",
                    "created_at": "2025-05-14T00:00:00Z",
                    "display_name": "Claude Opus 4.6",
                    "model": "anthropic/claude-opus-4.6",
                    "provider": "anthropic",
                    "updated_at": "2026-03-01T00:00:00Z",
                    "vendors": {
                        "anthropic": {
                            "availability_status": "available",
                            "capabilities": {
                                "input": ["text", "image"],
                                "output": ["text", "tool_use"],
                                "streaming": true,
                                "supports_structured_outputs": true,
                                "supports_tool_calling": true,
                                "supports_tool_choice": true
                            },
                            "context_window": 1_000_000,
                            "launch_date": "2025-05-14",
                            "max_output_tokens": 32_768,
                            "pricing": {
                                "currency": "USD",
                                "input_per_million": 2.5,
                                "output_per_million": 10.0,
                                "flex": {
                                    "input_per_million": 1.25,
                                    "output_per_million": 5.0
                                }
                            },
                            "service_tiers": ["standard", "flex"],
                            "future_flag": true
                        }
                    },
                    "future_model_field": "kept"
                }
            ],
            "has_more": true,
            "next_cursor": "cursor_2",
            "future_envelope_field": {"page": 1}
        });

        let response: MergeModelCatalogueResponse = serde_json::from_value(payload).expect("catalogue payload");
        response.validate_envelope().expect("catalogue envelope");
        assert!(response.has_more);
        assert_eq!(response.next_cursor.as_deref(), Some("cursor_2"));
        assert_eq!(response.extra.get("future_envelope_field").expect("extra envelope"), &json!({"page": 1}));

        let record = &response.data[0];
        assert_eq!(record.model, "anthropic/claude-opus-4.6");
        assert_eq!(record.display_name.as_deref(), Some("Claude Opus 4.6"));
        assert_eq!(record.extra.get("future_model_field").expect("extra model"), &json!("kept"));

        let vendor = record.vendors.get("anthropic").expect("vendor info");
        assert_eq!(vendor.availability_status, Some(MergeAvailabilityStatus::Available));
        assert_eq!(vendor.capabilities.input, vec![MergeInputModality::Text, MergeInputModality::Image]);
        assert_eq!(vendor.capabilities.output, vec![MergeOutputModality::Text, MergeOutputModality::ToolUse]);
        assert!(vendor.capabilities.streaming);
        assert!(vendor.capabilities.supports_structured_outputs);
        assert!(vendor.capabilities.supports_tool_calling);
        assert!(vendor.capabilities.supports_tool_choice);
        assert_eq!(vendor.context_window, Some(1_000_000));
        assert_eq!(vendor.max_output_tokens, Some(32_768));
        assert_eq!(vendor.service_tiers, vec![MergeServiceTier::Standard, MergeServiceTier::Flex]);
        assert_eq!(vendor.pricing.currency, "USD");
        assert!((vendor.pricing.input_per_million - 2.5).abs() < f64::EPSILON);
        assert!((vendor.pricing.output_per_million - 10.0).abs() < f64::EPSILON);
        assert!((vendor.pricing.flex.as_ref().expect("flex pricing").input_per_million - 1.25).abs() < f64::EPSILON);
        assert!((vendor.pricing.flex.as_ref().expect("flex pricing").output_per_million - 5.0).abs() < f64::EPSILON);
        assert_eq!(vendor.extra.get("future_flag").expect("extra vendor"), &json!(true));
    }

    #[test]
    fn merge_models_catalogue_rejects_wrong_envelope() {
        let response: MergeModelCatalogueResponse = serde_json::from_value(json!({
            "object": "response",
            "data": []
        }))
        .expect("catalogue payload");

        assert!(response.validate_envelope().is_err());
    }

    #[test]
    fn merge_responses_round_trip_text_tool_call_and_stream_payloads() {
        let request = MergeResponsesRequest {
            input: vec![MergeResponsesInputItem::message(
                MergeMessageRole::User,
                MergeResponsesInputContent::Text("What's the weather in San Francisco?".into()),
            )],
            model: Some("anthropic/claude-sonnet-4-20250514".into()),
            tools: Some(vec![MergeToolDefinition::function(
                "get_weather",
                "Get the current weather for a location.",
                json!({
                    "type": "object",
                    "properties": {
                        "location": {
                            "type": "string"
                        }
                    }
                }),
            )]),
            tool_choice: Some(MergeToolChoice::Mode(MergeToolChoiceMode::Auto)),
            response_format: Some(MergeResponseFormat::JsonSchema {
                kind: MergeResponseFormatType::JsonSchema,
                json_schema: MergeJsonSchemaResponseFormat {
                    name: "person".into(),
                    schema: json!({
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "age": {"type": "integer"},
                            "name": {"type": "string"}
                        },
                        "required": ["name", "age"]
                    }),
                    strict: Some(true),
                    extra: Map::new(),
                },
            }),
            include_routing_metadata: true,
            service_tier: Some(MergeServiceTier::Flex),
            stream: true,
            ..Default::default()
        };

        let request_json = serde_json::to_value(&request).expect("request json");
        assert_eq!(request_json["input"][0]["type"], "message");
        assert_eq!(request_json["input"][0]["role"], "user");
        assert_eq!(request_json["tools"][0]["type"], "function");
        assert_eq!(request_json["tool_choice"], "auto");
        assert_eq!(request_json["response_format"]["type"], "json_schema");
        assert_eq!(request_json["include_routing_metadata"], true);
        assert_eq!(request_json["service_tier"], "flex");
        assert_eq!(request_json["stream"], true);

        let response: MergeResponsesResponse = serde_json::from_value(json!({
            "id": "resp_b2c3d4e5f6a7",
            "created_at": "2026-03-23T12:03:00Z",
            "model": "openai/gpt-5.1",
            "output": [
                {
                    "id": "msg_004",
                    "content": [
                        {
                            "type": "text",
                            "text": "{\"name\": \"Ada Lovelace\", \"age\": 36}"
                        }
                    ],
                    "type": "message",
                    "role": "assistant",
                    "finish_reason": "stop"
                },
                {
                    "id": "msg_005",
                    "content": [
                        {
                            "type": "tool_use",
                            "id": "call_abc123",
                            "input": {"location": "San Francisco"},
                            "name": "get_weather"
                        }
                    ],
                    "type": "message",
                    "role": "assistant",
                    "finish_reason": "tool_use"
                }
            ],
            "usage": {
                "input_tokens": 21,
                "output_tokens": 12,
                "total_tokens": 33,
                "cost": 0.000031
            },
            "object": "response",
            "vendor": "openai",
            "provider_request_id": "chatcmpl-ghi789",
            "routing": {
                "vendor": "openai",
                "request_id": "req_123",
                "latency_ms": 42
            },
            "service_tier": "flex"
        }))
        .expect("response payload");

        response.validate_envelope().expect("response envelope");
        assert_eq!(response.vendor.as_deref(), Some("openai"));
        assert_eq!(response.provider_request_id.as_deref(), Some("chatcmpl-ghi789"));
        assert_eq!(response.service_tier, Some(MergeServiceTier::Flex));
        assert_eq!(response.routing.as_ref().and_then(|routing| routing.request_id.as_deref()), Some("req_123"));
        assert_eq!(response.usage.total_tokens, 33);

        let first_output = match &response.output[0] {
            MergeResponseOutputItem::Message(message) => message,
            MergeResponseOutputItem::Raw(_) => panic!("expected message output"),
        };
        assert_eq!(first_output.role, MergeMessageRole::Assistant);
        assert_eq!(first_output.finish_reason, Some(MergeFinishReason::Stop));
        match &first_output.content[0] {
            MergeResponseContentPart::Text(text) => assert_eq!(text.text, "{\"name\": \"Ada Lovelace\", \"age\": 36}"),
            other => panic!("unexpected content part: {other:?}"),
        }

        let tool_output = match &response.output[1] {
            MergeResponseOutputItem::Message(message) => message,
            MergeResponseOutputItem::Raw(_) => panic!("expected message output"),
        };
        assert_eq!(tool_output.finish_reason, Some(MergeFinishReason::ToolUse));
        match &tool_output.content[0] {
            MergeResponseContentPart::ToolUse(tool_use) => {
                assert_eq!(tool_use.id, "call_abc123");
                assert_eq!(tool_use.name, "get_weather");
                assert_eq!(tool_use.input["location"], "San Francisco");
            }
            other => panic!("unexpected tool call content: {other:?}"),
        }

        let sse_event: MergeResponsesSseEvent = serde_json::from_value(json!({
            "event": "response.output_text.delta",
            "id": "evt_1",
            "data": {
                "delta": "Hello",
                "vendor": "openai",
                "provider_request_id": "chatcmpl-abc123"
            }
        }))
        .expect("sse payload");

        assert_eq!(sse_event.event, "response.output_text.delta");
        assert_eq!(sse_event.id.as_deref(), Some("evt_1"));
        assert_eq!(sse_event.data.as_ref().and_then(|data| data.delta.as_deref()), Some("Hello"));
        assert_eq!(sse_event.data.as_ref().and_then(|data| data.vendor.as_deref()), Some("openai"));
        assert_eq!(
            sse_event.data.as_ref().and_then(|data| data.provider_request_id.as_deref()),
            Some("chatcmpl-abc123")
        );
    }
}
