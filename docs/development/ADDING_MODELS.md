# Adding New Models to VT Code

This guide documents the complete workflow for adding a new LLM model to VT Code. Follow these steps to ensure all systems are properly configured.

## Overview

Adding a model requires updates in **three layers**:

1. **Constants Layer** - Model strings & metadata
2. **Configuration Layer** - Model palette configuration
3. **Core Layer** - Runtime model resolution & capabilities

For a new first-class provider, extend this workflow with provider enum,
configuration, factory registration, resolver, startup defaults, picker
presets, and a provider guide. NVIDIA is an OpenAI-compatible provider, but
its curated constants are intentionally separate from the OpenAI constants;
explicit NVIDIA model IDs remain valid even when they are not in the picker.
Gateway providers such as Merge Gateway use the same shared Chat Completions
transport while keeping curated picker entries separate from arbitrary valid
provider/model route IDs. Gateway integrations must document which native
features are intentionally not projected into the compatibility layer.
For marketplace entries such as Meta Muse on OpenRouter, add generated metadata
to both `docs/models.json` and the embedded
`crates/codegen/vtcode-config/build_data/openrouter_models.json`; keep the
official Meta provider's bare model IDs separate from OpenRouter's `meta/...`
namespace.

## Quick Checklist

- [ ] Add to the provider constants module (for example, NVIDIA uses `constants/models/nvidia.rs`)
- [ ] Add to model metadata (`docs/models.json`)
- [ ] For a gateway provider, document the default endpoint, API-key variable, curated picker routes, and pass-through rules for arbitrary provider/model IDs
- [ ] If the model is OpenRouter-only, mirror its metadata in `build_data/openrouter_models.json`
- [ ] Add enum variant (`crates/codegen/vtcode-config/src/models/model_id/mod.rs`)
- [ ] Update `as_str.rs` - string mapping
- [ ] Update `display.rs` - human-readable name
- [ ] Update `description.rs` - model description
- [ ] Update `parse.rs` - string parsing
- [ ] Update `collection.rs` - all_models list
- [ ] Update `capabilities.rs` - generation version
- [ ] Update `provider.rs` - provider assignment
- [ ] Verify with `cargo check --package vtcode-config`

## Detailed Steps

### Step 1: Add to Provider Constants

**File:** `crates/codegen/vtcode-config/src/constants/models/<provider>.rs`

```rust
// In the provider's SUPPORTED_MODELS array
pub const SUPPORTED_MODELS: &[&str] = &[
    // ... existing models
    "gpt-5.6-luna",    // Add here in order
    "gpt-5.6-luna",
];

// Add convenience constant (at bottom)
pub const GPT_5_6_LUNA: &str = "gpt-5.6-luna";
pub const GPT_5_6_LUNA: &str = "gpt-5.6-luna";
```

**When to update:**

- `SUPPORTED_MODELS` - always, for API availability
- `RESPONSES_API_MODELS` - if supports OpenAI Responses API
- `REASONING_MODELS` - if supports reasoning parameter
- `SERVICE_TIER_MODELS` - if supports service_tier parameter
- `TOOL_UNAVAILABLE_MODELS` - if NO tool calling support
- `HARMONY_MODELS` - if uses harmony tokenization (OSS models only)

### Step 2: Add to Model Metadata (models.json)

**File:** `docs/models.json`

```json
"gpt-5.6-luna": {
  "id": "gpt-5.6-luna",
  "name": "GPT-5.4 Nano",
  "description": "Lightweight variant optimized for speed and cost",
  "reasoning": false,
  "tool_call": true,
  "modalities": {
    "input": ["text"],
    "output": ["text"]
  },
  "context": 100000
}
```

**Fields to set:**

- `id` - matches constant name
- `name` - user-facing display name
- `description` - brief capability summary
- `reasoning` - has reasoning_effort support
- `tool_call` - supports function calling
- `modalities.input` - ["text"] or ["text", "image"] etc
- `modalities.output` - typically ["text"]
- `context` - context window size

Verify JSON: `python3 -m json.tool docs/models.json > /dev/null`

### Step 3: Add Enum Variant (model_id.rs)

**File:** `crates/codegen/vtcode-config/src/models/model_id/mod.rs`

Add in the appropriate provider section (OpenAI, Anthropic, etc.):

```rust
/// GPT-5.4 Nano - Lightweight GPT-5.4 variant optimized for speed and cost-efficiency
GPT56Luna,
/// GPT-5.4 Mini - Compact GPT-5.4 variant for cost-effective tasks
GPT56Luna,
```

**Naming convention:** `PascalCase` enum variant, no hyphens.

### Step 4: Update as_str.rs

**File:** `crates/codegen/vtcode-config/src/models/model_id/as_str.rs`

Maps enum to constant string:

```rust
ModelId::GPT56Luna => models::openai::GPT_5_6_LUNA,
ModelId::GPT56Luna => models::openai::GPT_5_6_LUNA,
```

### Step 5: Update display.rs

**File:** `crates/codegen/vtcode-config/src/models/model_id/display.rs`

Human-readable name for UI:

```rust
ModelId::GPT56Luna => "GPT-5.4 Nano",
ModelId::GPT56Luna => "GPT-5.4 Mini",
```

### Step 6: Update description.rs

**File:** `crates/codegen/vtcode-config/src/models/model_id/description.rs`

Full description for help/info:

```rust
ModelId::GPT56Luna => {
    "Lightweight GPT-5.4 variant optimized for speed and cost-efficiency"
}
ModelId::GPT56Luna => {
    "Compact GPT-5.4 variant for cost-effective tasks with reduced reasoning overhead"
}
```

### Step 7: Update parse.rs

**File:** `crates/codegen/vtcode-config/src/models/model_id/parse.rs`

String → Enum parsing:

```rust
s if s == models::openai::GPT_5_6_LUNA => Ok(ModelId::GPT56Luna),
s if s == models::openai::GPT_5_6_LUNA => Ok(ModelId::GPT56Luna),
```

### Step 8: Update collection.rs

**File:** `crates/codegen/vtcode-config/src/models/model_id/collection.rs`

Add to `all_models()` vector (keep alphabetically sorted within provider):

```rust
ModelId::GPT56Sol,
ModelId::GPT56Sol,
ModelId::GPT56Luna,      // Add here
ModelId::GPT56Luna,      // Add here
ModelId::GPT56Sol,
```

### Step 9: Update capabilities.rs

**File:** `crates/codegen/vtcode-config/src/models/model_id/capabilities.rs`

Update methods that match on model families:

```rust
// non_reasoning_variant() - if not a reasoning model
ModelId::GPT52 | ModelId::GPT56Sol | ModelId::GPT56Sol | ModelId::GPT56Luna | ModelId::GPT56Luna | ModelId::GPT5 => {
    Some(ModelId::GPT5Mini)
}

// generation() - version string
ModelId::GPT56Sol | ModelId::GPT56Sol | ModelId::GPT56Luna | ModelId::GPT56Luna => "5.4",

// is_top_tier() - if flagship class (optional, depends on model positioning)
// is_pro_variant() - if pro/advanced variant (optional)
// is_efficient_variant() - if lightweight/fast variant (optional)
// supports_shell_tool() - if supports shell execution (depends on model class)
```

### Step 10: Update provider.rs

**File:** `crates/codegen/vtcode-config/src/models/model_id/provider.rs`

Add to provider match:

```rust
ModelId::GPT5
 | ModelId::GPT52
 | ModelId::GPT52Codex
 | ModelId::GPT56Sol
 | ModelId::GPT56Sol
 | ModelId::GPT56Luna    // Add here
 | ModelId::GPT56Luna    // Add here
 | ModelId::GPT5Mini
 | ModelId::GPT5Nano
 // ... rest
 => Provider::OpenAI,
```

## Verification

After all changes, verify compilation:

```bash
cargo check --package vtcode-config
cargo check --all-targets
cargo clippy --workspace --all-targets -- -D warnings
```

Test model resolution:

```bash
# Verify model is in palette
cargo run -- /model --help | grep -i "gpt-5.6-sol"

# Test direct model selection
cargo run -- ask --model gpt-5.4-nano "test"
```

## Template for Copy-Paste

When adding a new model, use this template:

```
Model Name: gpt-5.4-nano
Enum Name: GPT56Luna
Provider: OpenAI
Generation: 5.4
Context: 100000
Reasoning: false
Tool Call: true
Input: ["text"]

--- Files to Update ---
1. openai.rs - SUPPORTED_MODELS + constant
2. models.json - full metadata
3. model_id.rs - enum variant
4. as_str.rs - ModelId::GPT56Luna => models::openai::GPT_5_6_LUNA
5. display.rs - "GPT-5.4 Nano"
6. description.rs - description string
7. parse.rs - s if s == models::openai::GPT_5_6_LUNA => Ok(ModelId::GPT56Luna)
8. collection.rs - add to all_models()
9. capabilities.rs - update version + optional trait methods
10. provider.rs - add to OpenAI provider match
```

## Automation Ideas

### Bash Script (Future Enhancement)

Could create `scripts/add_model.sh`:

- Prompt for model details (name, provider, context, etc.)
- Generate code snippets
- Auto-insert into files at proper locations
- Run cargo check

### Build Script (build.rs)

The `build.rs` generates model capabilities from `docs/models.json`. Ensure JSON is valid before running build.

### Testing

Add model to integration test:

```rust
#[test]
fn test_gpt_5_4_nano_parsing() {
    let model = "gpt-5.6-luna".parse::<ModelId>().unwrap();
    assert_eq!(model, ModelId::GPT56Luna);
    assert_eq!(model.provider(), Provider::OpenAI);
    assert_eq!(model.generation(), "5.4");
}
```

## Common Mistakes

x **Don't:**

- Add model only to JSON without enum
- Use hyphens in enum names (`GPT-5-4-Nano`)
- Forget to update `provider.rs` match
- Forget to update `collection.rs` all_models list
- Inconsistent naming across files

v **Do:**

- Keep naming consistent: `gpt-5.4-nano` (const), `GPT56Luna` (enum), `"GPT-5.4 Nano"` (display)
- Update all 10 files in order
- Run `cargo check` after each logical group
- Test with actual model resolution before submitting

## Related Files

- Provider setup: `docs/providers/PROVIDER_GUIDES.md`
- Provider quick references: `docs/providers/<provider>-quick-reference.md`
- Configuration precedence: `docs/config/CONFIGURATION_PRECEDENCE.md`
- Model examples: `docs/models.json`
- Constants reference: `crates/codegen/vtcode-config/src/constants/models/`
