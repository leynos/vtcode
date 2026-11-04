import test from "node:test";
import assert from "node:assert/strict";
import { InMemoryBackend, MAX_TURN_PROMPT_BYTES, DiapasonBackend, buildTurnPrompt, createUnifiedDiff, digest } from "../src/backend.ts";
import {
  BROWSER_SETTINGS_STORAGE_KEY,
  BROWSER_WORKSPACE_STORAGE_KEY,
  loadBrowserSettings,
  loadBrowserState,
  saveBrowserSettings,
  saveBrowserState,
} from "../src/persistence.ts";
import { isRecord, type BackendConnectionEvent, type StatusPayload } from "../src/types.ts";
import type { StorageLike } from "../src/persistence.ts";

class MemoryStorage implements StorageLike {
  #values = new Map<string, string>();

  getItem(key: string): string | null { return this.#values.get(key) ?? null; }
  setItem(key: string, value: string): void { this.#values.set(key, value); }
  removeItem(key: string): void { this.#values.delete(key); }
}

interface FakeMessageEvent {
  readonly data: unknown;
}

type FakeMessageHandler = (event: FakeMessageEvent) => void;

function requestRecord(serialized: string): Record<string, unknown> {
  const request: unknown = JSON.parse(serialized);
  if (!isRecord(request)) throw new Error("fake WebSocket received a non-object request");
  return request;
}

class LoopbackWebSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;
  static instances: LoopbackWebSocket[] = [];
  static requests: Record<string, unknown>[] = [];

  readyState = LoopbackWebSocket.CONNECTING;
  onopen: (() => void) | null = null;
  onmessage: FakeMessageHandler | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;

  constructor(_url: string | URL, _protocols?: string | string[]) {
    LoopbackWebSocket.instances.push(this);
    queueMicrotask(() => {
      this.readyState = LoopbackWebSocket.OPEN;
      this.onopen?.();
    });
  }

  send(serialized: string): void {
    const request = requestRecord(serialized);
    LoopbackWebSocket.requests.push(request);
    const payload = this.responsePayload(request);
    queueMicrotask(() => {
      if (this.readyState === LoopbackWebSocket.OPEN) {
        this.onmessage?.({ data: JSON.stringify({ type: "response", request_id: request.request_id, ok: true, payload }) });
      }
    });
  }

  protected responsePayload(request: Record<string, unknown>): unknown {
    if (request.type === "pair") return { token: "session-token", protocol_version: "1", expires_in_secs: 300 };
    if (request.type === "turn.request") return { accepted: true, turn_id: "turn-1", output: "accepted" };
    return {
      protocol_version: "1",
      connected: true,
      authenticated_origin: "http://localhost:5173",
      runtime: {
        workspace_root: "/workspace",
        connected: true,
        turns_available: true,
        mutations_allowed: true,
        checks_allowed: true,
        approval_authority: "terminal",
      },
      settings: {
        host: "127.0.0.1",
        port: 4321,
        pairing_ttl_secs: 300,
        max_frame_bytes: 1048576,
        max_in_flight_requests: 8,
        remote_enabled: false,
      },
      latest_sequence: 0,
    } satisfies StatusPayload;
  }

  emitFrame(frame: unknown): void {
    if (this.readyState === LoopbackWebSocket.OPEN) this.onmessage?.({ data: frame });
  }

  close(): void {
    if (this.readyState === LoopbackWebSocket.CLOSED) return;
    this.readyState = LoopbackWebSocket.CLOSED;
    this.onclose?.();
  }
}

class MalformedResponseWebSocket extends LoopbackWebSocket {
  override send(serialized: string): void {
    const request = requestRecord(serialized);
    if (request.type === "pair") {
      super.send(serialized);
      return;
    }
    queueMicrotask(() => {
      if (this.readyState === LoopbackWebSocket.OPEN) this.onmessage?.({ data: "{" });
    });
  }
}

class InvalidPayloadWebSocket extends LoopbackWebSocket {
  protected override responsePayload(request: Record<string, unknown>): unknown {
    return request.type === "status" ? {} : super.responsePayload(request);
  }
}

class InvalidEnvelopeWebSocket extends LoopbackWebSocket {
  override send(serialized: string): void {
    const request = requestRecord(serialized);
    if (request.type === "pair") {
      super.send(serialized);
      return;
    }
    queueMicrotask(() => {
      if (this.readyState === LoopbackWebSocket.OPEN) {
        this.onmessage?.({ data: JSON.stringify({ type: "response", request_id: request.request_id, ok: true, payload: {}, error: { code: "bad", message: "bad" } }) });
      }
    });
  }
}

function installWebSocket(implementation: typeof LoopbackWebSocket): typeof WebSocket | undefined {
  const previous = globalThis.WebSocket as typeof WebSocket | undefined;
  globalThis.WebSocket = implementation as unknown as typeof WebSocket;
  return previous;
}

function restoreWebSocket(previous: typeof WebSocket | undefined): void {
  if (previous) globalThis.WebSocket = previous;
  else Reflect.deleteProperty(globalThis, "WebSocket");
}

function instanceAt(index: number): LoopbackWebSocket {
  const instance = LoopbackWebSocket.instances[index];
  assert.ok(instance);
  return instance;
}

function requestAt(index: number): Record<string, unknown> {
  const request = LoopbackWebSocket.requests[index];
  assert.ok(request);
  return request;
}

function nextMicrotask(): Promise<void> {
  return new Promise((resolve) => queueMicrotask(resolve));
}

function runtimeEventFrame(sequence: number, event: Record<string, unknown> = { type: "error", message: "runtime" }): string {
  return JSON.stringify({ type: "event", sequence, event: { schema_version: "1", event } });
}

test("fallback backend is deterministic and memory-only", async () => {
  const backend = new InMemoryBackend({ "src/main.js": "export const answer = 42;\n" });
  assert.deepEqual(await backend.listFiles(), ["src/main.js"]);
  const file = await backend.readFile("src/main.js");
  assert.equal(file.content, "export const answer = 42;\n");
  assert.equal(file.digest, await digest(file.content));
  await backend.writeFile({ path: file.path, content: "export const answer = 43;\n", baseDigest: file.digest });
  assert.equal((await backend.readFile(file.path)).content, "export const answer = 43;\n");
});

test("fallback backend rejects stale proposals", async () => {
  const backend = new InMemoryBackend({ "README.md": "base\n" });
  const base = await backend.readFile("README.md");
  await backend.writeFile({ path: "README.md", content: "new base\n", baseDigest: base.digest });
  await assert.rejects(
    backend.writeFile({ path: "README.md", content: "stale patch\n", baseDigest: base.digest }),
    /Stale patch/,
  );
});

test("fallback backend validates malformed proposal entries", async () => {
  const backend = new InMemoryBackend({ "README.md": "base\n" });
  await assert.rejects(backend.proposeChanges([null]), /Workspace path is invalid/);
});

test("fallback structured proposal supports diff, apply, checks, and guarded revert", async () => {
  const backend = new InMemoryBackend();
  const base = await backend.readFile("src/greeting.js");
  const changes = [{ path: base.path, base_digest: base.digest, content: base.content.replace("Hello", "Hi") }];
  const proposal = await backend.proposeChanges(changes);
  assert.match(proposal.unified_diff, /--- a\/src\/greeting\.js/);
  assert.match(createUnifiedDiff(changes, { "src/greeting.js": base.content }), /\+.*Hi/);
  const applied = await backend.applyProposal(proposal.proposal_id);
  assert.match((await backend.readFile(base.path)).content, /Hi/);
  assert.equal((await backend.runChecks()).exit_code, 1);
  await backend.revertLastChange(applied.change_id);
  assert.equal((await backend.readFile(base.path)).content, base.content);
});

test("unified diff preserves context and separates distant edits", () => {
  const before = Array.from({ length: 12 }, (_, index) => `line-${index + 1}`).join("\n") + "\n";
  const after = before.replace("line-2", "changed-2").replace("line-10", "changed-10");
  const diff = createUnifiedDiff([{ path: "file.txt", content: after }], { "file.txt": before });
  assert.equal(diff, [
    "--- a/file.txt",
    "+++ b/file.txt",
    "@@ -1,5 +1,5 @@",
    " line-1",
    "-line-2",
    "+changed-2",
    " line-3",
    " line-4",
    " line-5",
    "@@ -7,6 +7,6 @@",
    " line-7",
    " line-8",
    " line-9",
    "-line-10",
    "+changed-10",
    " line-11",
    " line-12",
  ].join("\n"));
});

test("unified diff reports additions, deletions, and missing final newlines", () => {
  const before = "one\ntwo\nthree\n";
  assert.equal(
    createUnifiedDiff([{ path: "middle.txt", content: "one\ninserted\ntwo\nthree\n" }], { "middle.txt": before }),
    [
      "--- a/middle.txt",
      "+++ b/middle.txt",
      "@@ -1,3 +1,4 @@",
      " one",
      "+inserted",
      " two",
      " three",
    ].join("\n"),
  );
  assert.equal(
    createUnifiedDiff([{ path: "middle.txt", content: "one\nthree\n" }], { "middle.txt": before }),
    [
      "--- a/middle.txt",
      "+++ b/middle.txt",
      "@@ -1,3 +1,2 @@",
      " one",
      "-two",
      " three",
    ].join("\n"),
  );
  assert.equal(
    createUnifiedDiff([{ path: "new.txt", content: "first\nsecond\n" }], { "new.txt": "" }),
    ["--- a/new.txt", "+++ b/new.txt", "@@ -0,0 +1,2 @@", "+first", "+second"].join("\n"),
  );
  assert.equal(
    createUnifiedDiff([{ path: "old.txt", content: "" }], { "old.txt": "first\nsecond\n" }),
    ["--- a/old.txt", "+++ b/old.txt", "@@ -1,2 +0,0 @@", "-first", "-second"].join("\n"),
  );
  assert.equal(
    createUnifiedDiff([{ path: "line.txt", content: "new" }], { "line.txt": "old" }),
    [
      "--- a/line.txt",
      "+++ b/line.txt",
      "@@ -1 +1 @@",
      "-old",
      "\\ No newline at end of file",
      "+new",
      "\\ No newline at end of file",
    ].join("\n"),
  );
});

test("fallback multi-file apply validates every file before mutating any file", async () => {
  const backend = new InMemoryBackend({ "a.txt": "a\n", "b.txt": "b\n" });
  const first = await backend.readFile("a.txt");
  const second = await backend.readFile("b.txt");
  const proposal = await backend.proposeChanges([
    { path: first.path, base_digest: first.digest, content: "new a\n" },
    { path: second.path, base_digest: second.digest, content: "new b\n" },
  ]);
  await backend.writeFile({ path: second.path, content: "external\n", baseDigest: second.digest });
  await assert.rejects(backend.applyProposal(proposal.proposal_id), /Stale patch/);
  assert.equal((await backend.readFile(first.path)).content, first.content);
  assert.equal((await backend.readFile(second.path)).content, "external\n");
});

test("fallback turn explains that no Diapason runtime is connected", async () => {
  const result = await new InMemoryBackend().requestTurn("review this draft");
  assert.equal(result.accepted, false);
  assert.match(result.reason ?? "", /agent turns require an active Diapason runtime/i);
});

test("turn prompt includes a bounded draft diff", () => {
  const prompt = buildTurnPrompt("Review the change", "--- a/file.js\n+++ b/file.js\n+updated\n".repeat(2000));
  assert.match(prompt, /Review the change/);
  assert.match(prompt, /browser draft unified diff/);
  assert.match(prompt, /diff truncated/);
  assert.ok(new TextEncoder().encode(prompt).length <= MAX_TURN_PROMPT_BYTES);
});

test("turn prompt stays bounded for multi-byte prompt text", () => {
  const prompt = buildTurnPrompt("🙂".repeat(MAX_TURN_PROMPT_BYTES), "diff");
  assert.ok(new TextEncoder().encode(prompt).length <= MAX_TURN_PROMPT_BYTES);
});

test("browser workspace state survives reloads within one Vite app instance", () => {
  const storage = new MemoryStorage();
  const state = {
    fallback_files: { "src/app.js": "export const ready = true;\n" },
    drafts: { "src/app.js": "export const ready = false;\n" },
    open_tabs: ["src/app.js"],
    selected: "src/app.js",
    expanded_dirs: ["src"],
    filter: "app",
    workspace_path: "/tmp/webmcp-app",
  };
  assert.equal(saveBrowserState(storage, "vite-1", state), true);
  assert.deepEqual(loadBrowserState(storage, "vite-1"), {
    version: 1,
    app_instance: "vite-1",
    ...state,
  });
  assert.equal(loadBrowserState(storage, "vite-2"), null);
  assert.equal(storage.getItem(BROWSER_WORKSPACE_STORAGE_KEY), null);
});

test("browser settings persist setup values but never pairing credentials", () => {
  const storage = new MemoryStorage();
  assert.equal(saveBrowserSettings(storage, "vite-1", {
    workspace_path: "/tmp/webmcp-app",
    bridge_url: "ws://127.0.0.1:4321/webmcp",
    pairing_code: "SECRET-CODE",
  }), true);
  assert.deepEqual(loadBrowserSettings(storage, "vite-1"), {
    version: 1,
    app_instance: "vite-1",
    workspace_path: "/tmp/webmcp-app",
    bridge_url: "ws://127.0.0.1:4321/webmcp",
  });
  const serializedSettings = storage.getItem(BROWSER_SETTINGS_STORAGE_KEY);
  assert.ok(serializedSettings);
  assert.equal(serializedSettings.includes("SECRET-CODE"), false);
  assert.equal(loadBrowserSettings(storage, "vite-2"), null);
  assert.equal(storage.getItem(BROWSER_SETTINGS_STORAGE_KEY), null);
});

test("websocket backend resumes the in-memory session after a dropped socket", async () => {
  const previousWebSocket = installWebSocket(LoopbackWebSocket);
  LoopbackWebSocket.instances = [];
  LoopbackWebSocket.requests = [];
  const backend = new DiapasonBackend("ws://127.0.0.1:4321/webmcp");
  try {
    await backend.pair("ABCD");
    assert.equal(backend.connected, true);
    assert.equal(backend.heartbeatIntervalMs(), 30_000);
    const statusUpdates: StatusPayload[] = [];
    const stopStatus = backend.subscribeToStatus((payload) => {
      if (payload) statusUpdates.push(payload);
    });
    const status = await backend.status();
    assert.equal(status.settings.port, 4321);
    const latestStatus = statusUpdates.at(-1);
    assert.ok(latestStatus);
    assert.equal(latestStatus.authenticated_origin, "http://localhost:5173");
    stopStatus();

    instanceAt(0).close();
    assert.equal(backend.connected, false);

    const resumedStatus = await backend.status();
    assert.equal(resumedStatus.connected, true);
    assert.equal(backend.connected, true);
    assert.equal(LoopbackWebSocket.instances.length, 2);

    await backend.requestTurn("Implement the staged change", "proposal-1");
    const request = requestAt(LoopbackWebSocket.requests.length - 1);
    assert.equal(request.type, "turn.request");
    assert.equal(request.token, "session-token");
    assert.equal(request.prompt, "Implement the staged change");
    assert.equal(request.proposal_id, "proposal-1");
  } finally {
    backend.close();
    restoreWebSocket(previousWebSocket);
  }
});

test("protocol errors invalidate the session and notify connection listeners", async () => {
  const previousWebSocket = installWebSocket(MalformedResponseWebSocket);
  const backend = new DiapasonBackend("ws://127.0.0.1:4321/webmcp");
  const connectionStates: BackendConnectionEvent[] = [];
  const stopConnection = backend.subscribeToConnection((event) => connectionStates.push(event));
  try {
    await backend.pair("ABCD");
    await assert.rejects(backend.status(), { code: "protocol_error" });
    assert.equal(backend.connected, false);
    assert.equal(backend.token, null);
    assert.equal(backend.connectionState, "reauthorize");
    const latestConnection = connectionStates.at(-1);
    assert.ok(latestConnection);
    assert.equal(latestConnection.state, "reauthorize");
  } finally {
    stopConnection();
    backend.close();
    restoreWebSocket(previousWebSocket);
  }
});

test("malformed response envelopes reject the pending request and fail closed", async () => {
  const previousWebSocket = installWebSocket(InvalidEnvelopeWebSocket);
  LoopbackWebSocket.instances = [];
  const backend = new DiapasonBackend("ws://127.0.0.1:4321/webmcp");
  try {
    await backend.pair("ABCD");
    await assert.rejects(backend.status(), { code: "protocol_error" });
    assert.equal(backend.connected, false);
    assert.equal(backend.token, null);
  } finally {
    backend.close();
    restoreWebSocket(previousWebSocket);
  }
});

test("invalid operation payloads reject the request and invalidate the session", async () => {
  const previousWebSocket = installWebSocket(InvalidPayloadWebSocket);
  LoopbackWebSocket.instances = [];
  const backend = new DiapasonBackend("ws://127.0.0.1:4321/webmcp");
  try {
    await backend.pair("ABCD");
    await assert.rejects(backend.status(), { code: "protocol_error" });
    assert.equal(backend.connected, false);
    assert.equal(backend.connectionState, "reauthorize");
  } finally {
    backend.close();
    restoreWebSocket(previousWebSocket);
  }
});

test("invalid event wrappers invalidate the authenticated session", async () => {
  const previousWebSocket = installWebSocket(LoopbackWebSocket);
  LoopbackWebSocket.instances = [];
  const backend = new DiapasonBackend("ws://127.0.0.1:4321/webmcp");
  try {
    await backend.pair("ABCD");
    instanceAt(0).emitFrame(runtimeEventFrame(1, { type: "error" }));
    await nextMicrotask();
    assert.equal(backend.connected, false);
    assert.equal(backend.token, null);
  } finally {
    backend.close();
    restoreWebSocket(previousWebSocket);
  }
});

test("runtime event sequences must remain contiguous after the first observed event", async () => {
  const previousWebSocket = installWebSocket(LoopbackWebSocket);
  LoopbackWebSocket.instances = [];
  const backend = new DiapasonBackend("ws://127.0.0.1:4321/webmcp");
  const events: number[] = [];
  const stopEvents = backend.subscribeToEvents((event) => {
    if (event.type === "event") events.push(event.sequence);
  });
  try {
    await backend.pair("ABCD");
    const socket = instanceAt(0);
    socket.emitFrame(runtimeEventFrame(5));
    await nextMicrotask();
    assert.deepEqual(events, [5]);
    assert.equal(backend.connected, true);
    socket.emitFrame(runtimeEventFrame(7));
    await nextMicrotask();
    assert.equal(backend.connected, false);
    assert.equal(backend.token, null);
  } finally {
    stopEvents();
    backend.close();
    restoreWebSocket(previousWebSocket);
  }
});
