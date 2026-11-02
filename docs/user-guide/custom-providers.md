# Custom Providers

Custom providers describe OpenAI-compatible endpoints that VT Code does not
ship a built-in profile for. Add them under `[custom_providers]` in
`vtcode.toml`. The field-by-field contract is the
[Configuration Field Reference](../config/CONFIG_FIELD_REFERENCE.md).

## Per-model profiles

Sparse per-model overrides use the literal model identifier under
`custom_providers.profiles."<model-id>"`:

```toml
[custom_providers.profiles."gpt-5.6-sol"]
api_format = "openai-responses"
context_window = 131072
supports_tools = true
```

A profile only adjusts runtime defaults for a model that is already selectable;
it does not make a model available. Profile values take precedence over
provider defaults, which take precedence over model metadata and conservative
fallbacks. An explicit `false` is honored even when a lower layer implies
`true`.

## Responses function-call ID remapping

By default VT Code rejects a terminal Responses function call whose `call_id`
differs from the ID streamed for that call, so arguments cannot be correlated
with the wrong call.

For a custom endpoint known to rewrite function-call IDs, opt in on the
provider or on one exact model profile:

```toml
[custom_providers.profiles."gpt-5.6-sol"]
api_format = "openai-responses"
responses_allow_function_call_id_remap = true
```

- **Default:** `false`.
- **Precedence:** model profile, then provider defaults. An explicit profile
  `false` overrides a provider-level `true`.
- **Safe-use limits:** compatibility mode accepts only a one-to-one match
  between unmatched ordinary function calls with the same non-empty name and
  identical, complete JSON arguments. It rejects duplicate IDs, duplicate
  semantic payloads, partial JSON, missing or contradictory calls, and
  custom/freeform calls. Leave the field absent or `false` for providers that
  preserve IDs.

The request policy and its verification boundaries are maintained in
[Model profiles and provider API format](../development/model-profiles.md) and
[Responses hardening](../development/responses-hardening.md).
