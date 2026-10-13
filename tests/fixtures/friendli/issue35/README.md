# Friendli function-call ID mismatch fixture

`function-response.sse` is the response body captured from a synthetic
Friendli Serverless OpenAI Responses request on 5 September 2026. The request
required the side-effect-free `fixture_echo` function with `{"text":"OK"}`.
No request headers, credentials, or user data are included.

The streamed function call uses `call_8b72964ed25d90d0`; the terminal
`response.completed` call uses `chatcmpl-tool-aec11d97912311ee`. Its function
name and strict JSON arguments are otherwise equal. Keep both IDs unchanged:
the fixture protects strict-default rejection and explicit compatibility-mode
reconciliation while ensuring the terminal ID remains authoritative.
