# Provider rate-limit headers in ACP

When a provider returns HTTP 429, VT Code includes recognized quota headers in
the ACP back-off notice. Missing or malformed optional values are ignored;
they do not prevent the normal retry policy from running. Zero remaining
requests or tokens is a meaningful value and is shown.

Each custom provider can map response header names to specific metrics under
`[custom_providers.rate_limit_headers]`. Header matching is case insensitive.
The defaults recognize Baseten's four per-minute headers, including when the
provider has a local alias such as `baseten-glm`. Known Fireworks and Together
provider names also select their provider-specific defaults. Explicit mappings
allow other aliases and gateways to use the same metrics.

```toml
# Place this table after the corresponding [[custom_providers]] entry.
[custom_providers.rate_limit_headers]
requests_limit_per_minute = "x-ratelimit-limit-requests"
requests_remaining_per_minute = "x-ratelimit-remaining-requests"
tokens_limit_per_minute = "x-ratelimit-limit-tokens"
tokens_remaining_per_minute = "x-ratelimit-remaining-tokens"
```

For a Fireworks provider, use:

```toml
[custom_providers.rate_limit_headers]
prompt_tokens_limit_per_second = "X-Ratelimit-Limit-Tokens-Prompt"
cache_adjusted_prompt_tokens_limit_per_second = "X-Ratelimit-Limit-Tokens-Cache-Adjusted-Prompt"
generated_tokens_limit_per_second = "X-Ratelimit-Limit-Tokens-Generated"
prompt_tokens = "fireworks-prompt-tokens"
cached_prompt_tokens = "fireworks-cached-prompt-tokens"
```

Fireworks' limits are measured in tokens per second. Its `fireworks-*-tokens`
headers describe usage for the individual request, not remaining quota. The
notice preserves that distinction. See the provider's
[rate-limit documentation](https://docs.fireworks.ai/serverless/rate-limits)
and [response-header reference](https://docs.fireworks.ai/serverless/overview).

For a Together provider, use:

```toml
[custom_providers.rate_limit_headers]
requests_limit_per_second = "x-ratelimit-limit"
requests_remaining_per_second = "x-ratelimit-remaining"
tokens_limit_per_second = "x-tokenlimit-limit"
tokens_remaining_per_second = "x-tokenlimit-remaining"
reset_after_seconds = "x-ratelimit-reset"
```

Together's reset header is a suggested interval in seconds, not a Unix
timestamp. Its [header reference](https://docs.together.ai/docs/serverless/rate-limits)
defines the request and token limits per second. Fractional reset intervals
are rounded upward to milliseconds.

## Retry timing

The standard `Retry-After` header is always recognized independently of the
provider mappings. It accepts seconds, fractional seconds, or an HTTP date.
When both `Retry-After` and a configured reset interval are valid, the longer
interval is used. Invalid values fall back to the ordinary retry policy.

For a generation's retry sequence, the base delay is the greater of the
configured initial back-off and the largest provider minimum seen so far.
Delays grow exponentially from that base. For example, with a 10-second local
base and `Retry-After: 15`, retries wait at least 15, 30, and 60 seconds.
The remembered minimum survives later failures that omit the header and
resets for the next generation, including the next tool-loop segment.

Jitter can add time but cannot shorten the minimum. The local maximum
back-off caps the ordinary delay curve; it cannot shorten the provider's
exponential minimum. Extremely large intervals remain cancellable and do not
overflow the timer. Retry budgets and the rule against replaying a stream
after visible output still apply.

## ACP and Lody rendering

Notices use the existing standard `session/update` notification with
`sessionUpdate: "session_info_update"`. The extension data is:

```json
{
  "_meta": {
    "lody": {
      "notice": {
        "level": "warning",
        "source": "provider_rate_limit",
        "message": "baseten returned HTTP 429 (rate limited); request limit/min: 60; requests remaining/min: 0; VTCode will retry in 15.0s"
      }
    }
  }
}
```

The shape follows `LodyNotice` in
`Lody/acp-extension-core/src/session.ts` and the warning handler in
`Lody/apps/cli/src/agent/agent-client.ts`. Notices do not require a negotiated
rate-limit capability. Clients that do not consume this optional metadata
can ignore it; the diagnostics are not inserted into model conversation
history.

VT Code also advertises push-only `_meta.lody.rateLimits = { "version": 1 }`
and emits `_lody/rate_limits/update`, following
`Lody/acp-extension-core/src/rate-limits.ts`. Each quota record has a stable
`limitId`, provider/model scope, and a `limitName` retaining the absolute limit.
Complete limit/remaining pairs produce a bounded `usedPercent` and a 60-second
or 1-second window, according to the configured metric. A known request reset
interval supplies `resetsAtEpochSeconds` for Together's request window only;
otherwise the reset time is null. `Retry-After` is a retry instruction and is
never interpreted as a quota reset timestamp.

For limit-only Fireworks throughput headers, `windows` is empty: the ceiling
is known but utilization is not. Missing or inconsistent counts do not produce
invented utilization. Request prompt/cached-token counters stay in the notice
because reporting them as another usage delta would double count. No account
identity, wallet, plan, or quota query API is inferred from response headers.

## Validation

Unit tests cover partial/malformed headers, provider overrides, notice
rendering, HTTP dates, and retry budgets. Property tests exercise parsing and
the retry floor across varied inputs. ACP behavioral tests run the official
Rust client through initialize, session creation, and prompting against
scripted HTTP errors and successful streams. They inspect the same warning
metadata consumed by Lody.

The ignored VidaiMock physics tests use a local HTTP gateway to attach quota
headers to scripted 429 responses, then forward VidaiMock's actual stream.
The gateway is needed because the pinned VidaiMock provider schema cannot
configure arbitrary response headers. These tests observe real request
spacing and streamed recovery without using a paid provider.
