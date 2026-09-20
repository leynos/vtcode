# Configure provider rate-limit headers

Use this guide when a custom provider returns quota information in response
headers and you want VT Code to include it in retry notices and optional ACP
updates.

## Add a mapping to a custom provider

Add `rate_limit_headers` immediately after the associated
`[[custom_providers]]` entry. Map each field to the response header that your
provider actually sends:

```toml
[[custom_providers]]
name = "my-gateway"
display_name = "My gateway"
base_url = "https://llm.example/v1"
api_key_env = "MY_GATEWAY_API_KEY"

# Keep this table after the associated [[custom_providers]] entry.
[custom_providers.rate_limit_headers]
requests_limit_per_minute = "X-RateLimit-Limit-Requests"
requests_remaining_per_minute = "X-RateLimit-Remaining-Requests"
tokens_limit_per_minute = "X-RateLimit-Limit-Tokens"
tokens_remaining_per_minute = "X-RateLimit-Remaining-Tokens"
```

Header matching is case-insensitive. Do not map `Retry-After` here: VT Code
recognizes that header independently as a retry instruction.

## Defaults and provider-specific mappings

Custom providers start with four standard mappings for per-minute request and
token limits. Provider names that identify Fireworks or Together also receive
their provider-specific defaults. Values that you set in
`rate_limit_headers` take precedence for those fields, while other fields can
continue to use the effective provider defaults.

For the complete field list and provider mapping tables, see the [provider
rate-limit header developer reference](../development/provider-rate-limit-headers.md).

## Retry timing and quota resets

`Retry-After` controls when VT Code may retry a failed request. A configured
reset interval describes a provider quota window. `Retry-After` is never a
quota reset timestamp.

When both values are valid, VT Code uses the larger delay. Missing or malformed
values are ignored, and the normal retry policy remains in effect; any other
valid delay can still provide the retry minimum.

## ACP and Lody updates

For a provider HTTP 429, ACP receives the ordinary warning `session/update`
notice. VT Code can also push optional Lody rate-limit snapshots through
`_lody/rate_limits/update`, advertised by
`_meta.lody.rateLimits = { "version": 1 }`. These are push-only updates: there
is no rate-limit query API, and VT Code does not infer a provider account
identity from response headers.

## Incomplete or inconsistent headers

VT Code ignores missing or malformed header values. It does not invent
utilisation from a one-sided or inconsistent limit/remaining pair. A complete
limit and remaining pair can produce `usedPercent`; limit-only information may
be rendered as an absolute limit while utilisation and an unsupported window
remain absent.
