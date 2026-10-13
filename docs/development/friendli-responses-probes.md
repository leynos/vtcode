# Friendli Responses compatibility probes

The ignored `friendli_responses_selected_compatibility_probes` test is a paid,
explicitly selected compatibility probe. It sends synthetic prompts directly
to Friendli's Responses endpoint, never executes returned tools, and stores the
raw response plus a classified outcome in a caller-selected private directory.

## Safety contract

Set all four variables deliberately:

- `VTCODE_FRIENDLI_LIVE=1` enables paid traffic.
- `VTCODE_FRIENDLI_LIVE_KEY_FILE` names a file containing only the API key.
- `VTCODE_FRIENDLI_PROBE_CASES` selects one or more comma-separated cases.
- `VTCODE_FRIENDLI_CAPTURE_DIR` names a new, nonexistent output directory.

The probe atomically creates the capture directory with mode `0700` on Unix.
Before each request it writes the synthetic request body, then appends raw
response bytes as they arrive. Thus a connection or mid-stream read failure
still leaves an empty or partial capture and an explicit `transport_failure`
outcome. It also records HTTP status, content metadata, rate-limit response
headers, reported usage, and the strict outcome. It never records the
authorization header or key. Redirects are disabled so a redirect cannot
forward credentials or create an additional paid request. Each selected case
makes exactly one HTTP request; the test configures zero retries and rejects
duplicate cases, more than four cases, or a requested output budget above
10,000 tokens. Every selected case is attempted before the test reports their
aggregated compatibility failures.

Available cases are `buffered-text`, `streamed-text`, `function`,
`custom-required`, `custom-required-text-format`, `custom-named`, and
`custom-named-text-format`. Run only the cases needed for the investigation:

```bash
VTCODE_FRIENDLI_LIVE=1 \
VTCODE_FRIENDLI_LIVE_KEY_FILE=/private/path/friendli.key \
VTCODE_FRIENDLI_PROBE_CASES=function \
VTCODE_FRIENDLI_CAPTURE_DIR=/private/path/new-capture \
cargo nextest run --run-ignored only -p vtcode-llm \
  friendli_responses_selected_compatibility_probes
```

## Outcome rules

Text succeeds only when the trimmed completed output is exactly `OK`. Function success
requires exactly one terminal `fixture_echo` call, a nonempty final call ID,
and strict JSON arguments equal to `{"text":"OK"}`. Custom success requires
exactly one terminal `fixture_raw` call with raw input `OK` and a nonempty final
call ID.

An HTTP failure, malformed or incomplete response, changed call name/input,
or terminal prose without the requested call is failed compatibility. A 200
status alone is not success. The outcome file retains `no_call`,
`http_failure`, `transport_failure`, and `decode_failure` separately so later
reports cannot silently reinterpret them. Partial or non-UTF-8 response bytes
remain raw evidence and are never lossily decoded as a successful response.

The sanitized historical captures in
`tests/fixtures/friendli/issue36/` document one function-call success, two
custom HTTP 500 failures, and two named-custom prose/no-call completions.
Offline replay proves parser and client behaviour for those recorded shapes;
it does not prove that the current paid service behaves the same way.
