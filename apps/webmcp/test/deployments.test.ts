import assert from "node:assert/strict";
import test from "node:test";

import { WEBMCP_DEPLOYMENTS, browserOrigin, webmcpDeploymentForOrigin } from "../src/deployments.ts";

test("Diapason does not preset third-party hosted deployments", () => {
  assert.deepEqual(WEBMCP_DEPLOYMENTS, []);
  assert.equal(webmcpDeploymentForOrigin("https://diapason.example"), null);
  assert.equal(webmcpDeploymentForOrigin("https://example.test"), null);
});

test("browser origin preserves deployed hosts and falls back for non-browser execution", () => {
  assert.equal(browserOrigin({ origin: "https://diapason.example" }), "https://diapason.example");
  assert.equal(browserOrigin({ origin: "https://preview.diapason.example" }), "https://preview.diapason.example");
  assert.equal(browserOrigin({ origin: "" }), "http://localhost:5173");
  assert.equal(browserOrigin({ origin: "null" }), "http://localhost:5173");
  assert.equal(browserOrigin(null), "http://localhost:5173");
});
