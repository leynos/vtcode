import { createHash } from "node:crypto";
import { realpathSync } from "node:fs";
import { homedir } from "node:os";
import * as path from "node:path";

interface UserDataPathOptions {
    readonly environment?: NodeJS.ProcessEnv;
    readonly homeDirectory?: string;
    readonly platform?: NodeJS.Platform;
}

function platformPaths(options: UserDataPathOptions): typeof path.posix {
    return (options.platform ?? process.platform) === "win32" ? path.win32 : path.posix;
}

function userRoot(
    variable: "XDG_STATE_HOME" | "XDG_CACHE_HOME",
    fallback: readonly string[],
    options: UserDataPathOptions
): string {
    const paths = platformPaths(options);
    const configured = (options.environment ?? process.env)[variable];
    if (configured && paths.isAbsolute(configured)
        && (paths !== path.win32 || paths.parse(configured).root.length > 1)) {
        return configured;
    }
    return paths.join(options.homeDirectory ?? homedir(), ...fallback);
}

// Rust's Windows OsStr encoding is WTF-8: unpaired UTF-16 surrogates retain
// their three-byte encoding instead of becoming the UTF-8 replacement character.
function windowsPathBytes(value: string): Buffer {
    const bytes: number[] = [];
    for (const character of value) {
        const point = character.codePointAt(0);
        if (point === undefined) {
            continue;
        }
        if (point <= 0x7f) {
            bytes.push(point);
        } else if (point <= 0x7ff) {
            bytes.push(0xc0 | (point >> 6), 0x80 | (point & 0x3f));
        } else if (point <= 0xffff) {
            bytes.push(0xe0 | (point >> 12), 0x80 | ((point >> 6) & 0x3f), 0x80 | (point & 0x3f));
        } else {
            bytes.push(0xf0 | (point >> 18), 0x80 | ((point >> 12) & 0x3f), 0x80 | ((point >> 6) & 0x3f), 0x80 | (point & 0x3f));
        }
    }
    return Buffer.from(bytes);
}

// Hash identity only: match Rust's native-byte protocol independently of
// whether canonicalization retains a Windows verbatim namespace. Actual
// filesystem paths are never changed by this normalization.
function workspaceIdentityBytes(bytes: Uint8Array): Uint8Array {
    const namespacePrefix = [92, 92, 63, 92];
    const uncPrefix = [...namespacePrefix, 85, 78, 67, 92];
    if (bytes.length >= uncPrefix.length && uncPrefix.every((value, index) => bytes[index] === value)) {
        return Buffer.concat([Buffer.from([92, 92]), bytes.subarray(uncPrefix.length)]);
    }
    if (bytes.length >= namespacePrefix.length && namespacePrefix.every((value, index) => bytes[index] === value)) {
        return bytes.subarray(namespacePrefix.length);
    }
    return bytes;
}

/** Match the Rust workspace_state_dir digest of normalized canonical native bytes. */
export function workspaceStateDirectory(
    canonicalWorkspace: string | Uint8Array,
    options: UserDataPathOptions = {}
): string {
    const windows = (options.platform ?? process.platform) === "win32";
    const bytes = typeof canonicalWorkspace !== "string"
        ? canonicalWorkspace
        : windows
        ? windowsPathBytes(canonicalWorkspace)
        : Buffer.from(canonicalWorkspace, "utf8");
    const digest = createHash("sha256").update(workspaceIdentityBytes(bytes)).digest("hex");
    return platformPaths(options).join(
        userRoot("XDG_STATE_HOME", [".local", "state"], options),
        "diapason",
        "workspaces",
        digest
    );
}

/** Resolve the current workspace's trajectory without consulting old dotdirs. */
export function trajectoryLogPath(workspace: string): string {
    let canonical: string | Buffer;
    try {
        canonical = process.platform === "win32"
            ? realpathSync.native(workspace)
            : realpathSync.native(workspace, { encoding: "buffer" });
    } catch (error) {
        // Match Rust's fallback only for a workspace that does not exist yet.
        if (!(error instanceof Error) || !("code" in error) || error.code !== "ENOENT") {
            throw error;
        }
        const absolute = path.isAbsolute(workspace) ? workspace : path.resolve(workspace);
        canonical = process.platform === "win32" ? windowsPathBytes(absolute) : Buffer.from(absolute, "utf8");
    }
    return path.join(workspaceStateDirectory(canonical), "logs", "trajectory.jsonl");
}

/** Return the concrete cache path; TOML does not expand shell XDG variables. */
export function promptCacheDirectory(options: UserDataPathOptions = {}): string {
    return platformPaths(options).join(userRoot("XDG_CACHE_HOME", [".cache"], options), "diapason", "prompts");
}

/** Escape a concrete TOML basic string for use as a literal VS Code snippet. */
export function promptCacheCompletion(): string {
    const literal = `cache_dir = ${JSON.stringify(promptCacheDirectory())}`;
    return literal.replace(/[\\$}]/g, "\\$&");
}
