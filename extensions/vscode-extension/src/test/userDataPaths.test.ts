import * as assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, realpathSync, rmSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import * as path from "node:path";
import { test } from "node:test";
import { promptCacheDirectory, trajectoryLogPath, workspaceStateDirectory } from "../userDataPaths";

// Fixed digests were computed independently from the displayed native byte
// vectors, rather than by using this module to construct its own expectation.
const POSIX_WORKSPACE_DIGEST = "4f3f91f6b0b47e3bebd44da0bf3651775effa4a0bc22a35622a60cb34d4f962e";

test("canonical workspace state matches the fixed Rust SHA256 path vector", () => {
    assert.equal(
        workspaceStateDirectory("/srv/project", {
            platform: "linux", environment: { XDG_STATE_HOME: "/state" }, homeDirectory: "/home/tester",
        }),
        `/state/diapason/workspaces/${POSIX_WORKSPACE_DIGEST}`
    );
});

test("relative or empty XDG state roots fall back beneath the supplied home", () => {
    for (const environment of [{}, { XDG_STATE_HOME: "relative/state" }, { XDG_STATE_HOME: "" }]) {
        assert.equal(
            workspaceStateDirectory("/srv/project", { platform: "linux", environment, homeDirectory: "/home/tester" }),
            `/home/tester/.local/state/diapason/workspaces/${POSIX_WORKSPACE_DIGEST}`
        );
    }
});

test("Windows drive identity ignores the native namespace and matches the fixed digest", () => {
    // Normalized native bytes: 433a5c776f726b5c70726f6a656374.
    for (const workspace of ["C:\\work\\project", "\\\\?\\C:\\work\\project"]) {
        assert.equal(
            workspaceStateDirectory(workspace, {
                platform: "win32", environment: { XDG_STATE_HOME: "D:\\state" }, homeDirectory: "C:\\Users\\tester",
            }),
            "D:\\state\\diapason\\workspaces\\a7cff3d3569558eac890c2b09fb1adba00c6b2a6f7caadac374d6249c6e9e397"
        );
    }
});

test("Windows UNC identity matches the fixed digest with or without a native namespace", () => {
    // Normalized native bytes: 5c5c7365727665725c73686172655c70726f6a656374.
    for (const workspace of ["\\\\server\\share\\project", "\\\\?\\UNC\\server\\share\\project"]) {
        assert.equal(
            workspaceStateDirectory(workspace, {
                platform: "win32", environment: { XDG_STATE_HOME: "D:\\state" }, homeDirectory: "C:\\Users\\tester",
            }),
            "D:\\state\\diapason\\workspaces\\818c395a62da1a91f17af08384734c27fd6fe7822c5b68f45d69c9127b8d5ebb"
        );
    }
});

test("native byte namespace normalization applies independently of the host platform", () => {
    for (const bytes of ["433a5c776f726b5c70726f6a656374", "5c5c3f5c433a5c776f726b5c70726f6a656374"]) {
        assert.equal(
            workspaceStateDirectory(Buffer.from(bytes, "hex"), {
                platform: "linux", environment: { XDG_STATE_HOME: "/state" }, homeDirectory: "/home/tester",
            }),
            "/state/diapason/workspaces/a7cff3d3569558eac890c2b09fb1adba00c6b2a6f7caadac374d6249c6e9e397"
        );
    }
});

test("Windows drive-relative and root-relative XDG values use the home fallback", () => {
    for (const XDG_STATE_HOME of ["state", "C:state", "\\state", "/state"]) {
        assert.equal(
            workspaceStateDirectory("C:\\work\\project", {
                platform: "win32", environment: { XDG_STATE_HOME }, homeDirectory: "C:\\Users\\tester",
            }),
            "C:\\Users\\tester\\.local\\state\\diapason\\workspaces\\a7cff3d3569558eac890c2b09fb1adba00c6b2a6f7caadac374d6249c6e9e397"
        );
    }
});

test("Windows unpaired surrogates use WTF8 native bytes instead of replacement characters", () => {
    // Normalized native bytes: 433a5c776f726b5ceda080 (last three encode U+D800).
    for (const workspace of ["C:\\work\\\ud800", "\\\\?\\C:\\work\\\ud800"]) {
        assert.equal(
            workspaceStateDirectory(workspace, {
                platform: "win32", environment: { XDG_STATE_HOME: "D:\\state" }, homeDirectory: "C:\\Users\\tester",
            }),
            "D:\\state\\diapason\\workspaces\\8a3cf4ad866de901cc130aeb00d162bbf1ca8bc6a80d9e68ce3bf37823e3d8e2"
        );
    }
});

test("prompt cache uses an absolute XDG root and ignores a relative root", () => {
    assert.equal(promptCacheDirectory({ platform: "linux", environment: { XDG_CACHE_HOME: "/cache" }, homeDirectory: "/home/tester" }), "/cache/diapason/prompts");
    assert.equal(promptCacheDirectory({ platform: "linux", environment: { XDG_CACHE_HOME: "relative/cache" }, homeDirectory: "/home/tester" }), "/home/tester/.cache/diapason/prompts");
});

test("trajectory location canonicalizes workspace aliases and ends in the state log filename", () => {
    const root = mkdtempSync(path.join(tmpdir(), "diapason-paths-"));
    try {
        const workspace = path.join(root, "workspace");
        const alias = path.join(root, "alias");
        mkdirSync(workspace);
        symlinkSync(workspace, alias, process.platform === "win32" ? "junction" : "dir");
        const trajectory = trajectoryLogPath(alias);
        assert.equal(trajectory, trajectoryLogPath(realpathSync.native(workspace)));
        assert.equal(path.basename(trajectory), "trajectory.jsonl");
        assert.equal(path.basename(path.dirname(trajectory)), "logs");
        assert.ok(!trajectory.startsWith(alias + path.sep));
        assert.ok(!trajectory.startsWith(workspace + path.sep));
    } finally {
        rmSync(root, { recursive: true, force: true });
    }
});
