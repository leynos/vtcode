# WebMCP deployment reference

Diapason does not currently publish a hosted WebMCP browser app. Run the Vite
client locally or host a build yourself. The app derives its origin from
`window.location.origin`; configure that exact origin in the bridge allowlist.
The URL path is not part of the origin.

## Pair a self-hosted page

For a local app on the default Vite port:

```text
/webmcp pair http://localhost:5173
```

For a remote browser, host the client and bridge behind a TLS-terminating proxy
and allow the exact HTTPS origin. Neither the browser app nor the bridge uses a
wildcard origin.

The browser WebMCP interface and Diapason bridge are separate:

- Browser WebMCP registers bounded page-scoped tools when the browser supports
  `document.modelContext`.
- The authenticated `/webmcp` WebSocket connects the page to Diapason for
  workspace reads, reviewed proposals, runtime events and active agent turns.

A `DIAPASON TURN` requires a bridge paired with the TUI session that owns
terminal approval.

## Verification checklist

For either deployment:

1. Open the page directly and confirm the expected URL and fallback editor.
2. In a supported WebMCP browser, inspect the page and confirm the eight
   registered tools, bounded results, and `get_editor_state.webmcp_context`.
3. Start **Evidence** before a real inspector or ChatGPT in-app-browser run;
   keep the sanitized JSON beside the screenshot or recording.
4. For workspace access, pair using the exact origin table above and paste the
   URL/code printed by the same TUI.
5. Confirm browser edits remain proposals and that approval, apply, checks,
   and revert stay behind the Diapason terminal or configured headless policy.

The implementation details and test commands are in the
[WebMCP development guide](../development/webmcp.md), the
[browser bridge user guide](../user-guide/webmcp.md), and the
[WebMCP app guide](../../apps/webmcp/GUIDE.md).
