# Custom Provider Request Policy

Custom-provider request policy keeps ACP turns from overloading an
OpenAI-compatible endpoint. Configure it under the matching
`[[custom_providers]]` entry; see the [configuration guide](../config/config.md#custom_providers)
and [field reference](../config/CONFIG_FIELD_REFERENCE.md).

## Admission scope

Admission is both per provider and per VT Code process:

- Requests for every model routed through one configured provider share that
  provider's `max_in_flight_requests` limit.
- A request that reaches the limit waits for a permit for at most
  `queue_timeout_seconds`. If no permit becomes available, the request fails.
- Each configured provider has its own in-process limit. Separate VT Code
  processes do not share permits or counters, even when they use the same
  endpoint or configuration file. Enforce an installation-wide limit at the
  provider gateway when that is required.

Omit `max_in_flight_requests` to leave admission unrestricted. The other
defaults are a 600-second queue timeout, two retries, 500 ms initial backoff,
10,000 ms maximum backoff, and jitter enabled.

## Retry and output safety

For a transient provider failure, VT Code retries the request according to
`max_retries`, using bounded exponential backoff from
`retry_initial_backoff_ms` through `retry_max_backoff_ms`. Jitter can be
disabled with `retry_jitter = false`. A stream is retried only before it has
published text or reasoning. Once output is visible, VT Code surfaces a later
failure instead of risking duplicate output.

## Cancellation and turn safety

Cancellation is observed while a request waits for admission, sleeps between
retries, or is in flight. Cancellation releases any acquired permit and stops
further retries. A cancelled or superseded turn cannot publish a late response
into a later turn. The turn guard is released on every exit. The user request,
completed tool calls, and tool results remain in session history after an
exhausted network failure, so a follow-up such as `continue` retains gathered
context without replaying side effects.
