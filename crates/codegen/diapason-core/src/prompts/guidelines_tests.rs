//! Tests for runtime tool guideline generation and prompt-section refresh.
use super::*;

#[path = "guidelines_tests/api_contract.rs"]
mod api_contract;
#[path = "guidelines_tests/capability.rs"]
mod capability;
#[path = "guidelines_tests/guidance_budget.rs"]
mod guidance_budget;
#[path = "guidelines_tests/refresh.rs"]
mod refresh;
#[path = "guidelines_tests/runtime_sections.rs"]
mod runtime_sections;
#[path = "guidelines_tests/tool_guidance.rs"]
mod tool_guidance;
