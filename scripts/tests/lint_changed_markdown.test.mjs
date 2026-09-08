import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
    chmodSync,
    existsSync,
    mkdirSync,
    mkdtempSync,
    readFileSync,
    rmSync,
    symlinkSync,
    writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const TEST_DIRECTORY = dirname(fileURLToPath(import.meta.url));
const HELPER_PATH = join(TEST_DIRECTORY, "..", "lint_changed_markdown.mjs");
const CHANGED_FILES_ENV = "VTCODE_CHANGED_MARKDOWN_FILES_JSON";
const COMMAND_ENV = "VTCODE_MARKDOWNLINT_COMMAND";
const CAPTURE_ENV = "VTCODE_MARKDOWNLINT_CAPTURE";
const EXIT_STATUS_ENV = "VTCODE_MARKDOWNLINT_EXIT_STATUS";
const UNSUPPORTED_SYMLINK_CODES = new Set(["EACCES", "EPERM", "ENOTSUP"]);

function makeTemporaryDirectory() {
    return mkdtempSync(join(tmpdir(), "vtcode-lint-changed-markdown-"));
}

function makeCaptureExecutable(commandPath) {
    mkdirSync(dirname(commandPath), { recursive: true });
    writeFileSync(
        commandPath,
        `#!/usr/bin/env node
const { writeFileSync } = require("node:fs");

writeFileSync(process.env.${CAPTURE_ENV}, JSON.stringify(process.argv.slice(2)));
process.exitCode = Number(process.env.${EXIT_STATUS_ENV} ?? "0");
`,
    );
    chmodSync(commandPath, 0o755);
    return commandPath;
}

function makeFakeMarkdownlint(directory) {
    return makeCaptureExecutable(join(directory, "fake-markdownlint"));
}

function createFixtureFile(directory, relativePath) {
    const filePath = join(directory, relativePath);
    mkdirSync(dirname(filePath), { recursive: true });
    writeFileSync(filePath, "");
}

function runHelper(directory, rawChangedFiles, { exitStatus = 0, useDefaultCommand = false } = {}) {
    const capturePath = join(directory, "child-argv.json");
    const commandPath = useDefaultCommand
        ? makeCaptureExecutable(join(directory, "bin", "npx"))
        : makeFakeMarkdownlint(directory);
    const environment = {
        ...process.env,
        [CAPTURE_ENV]: capturePath,
        [EXIT_STATUS_ENV]: String(exitStatus),
    };

    if (useDefaultCommand) {
        delete environment[COMMAND_ENV];
        environment.PATH = `${dirname(commandPath)}${delimiter}${environment.PATH ?? ""}`;
    } else {
        environment[COMMAND_ENV] = commandPath;
    }

    if (rawChangedFiles === undefined) {
        delete environment[CHANGED_FILES_ENV];
    } else {
        environment[CHANGED_FILES_ENV] = rawChangedFiles;
    }

    return {
        result: spawnSync(process.execPath, [HELPER_PATH], {
            cwd: directory,
            encoding: "utf8",
            env: environment,
        }),
        capturePath,
    };
}

function assertRejectedBeforeChildLaunch({ rawChangedFiles, message, setup = () => {} }) {
    const directory = makeTemporaryDirectory();
    try {
        setup(directory);
        const { result, capturePath } = runHelper(directory, rawChangedFiles);

        assert.notEqual(result.status, 0);
        assert.match(result.stderr, message);
        assert.equal(existsSync(capturePath), false);
    } finally {
        rmSync(directory, { force: true, recursive: true });
    }
}

function skipUnsupportedSymlinkTest(testContext, error) {
    if (!UNSUPPORTED_SYMLINK_CODES.has(error?.code)) {
        return false;
    }

    testContext.skip("the host does not support creating symlinks");
    return true;
}

test("uses the fixed default npx argv and passes selected paths literally", () => {
    const directory = makeTemporaryDirectory();
    try {
        const markerPath = join(directory, "shell-marker");
        const selectedFiles = [
            "-leading-option.md",
            `docs/$(touch ${markerPath}) ; echo pwned \`echo tick\` & \"quoted\" 'single'.md`,
            "docs/glob[abc]*.md",
        ];
        for (const selectedFile of selectedFiles) {
            createFixtureFile(directory, selectedFile);
        }
        const { result, capturePath } = runHelper(directory, JSON.stringify(selectedFiles), {
            useDefaultCommand: true,
        });

        assert.equal(result.status, 0, result.stderr);
        assert.deepEqual(JSON.parse(readFileSync(capturePath, "utf8")), [
            "--yes",
            "--package",
            "markdownlint-cli2@0.23.2",
            "markdownlint-cli2",
            "--",
            ...selectedFiles,
        ]);
        assert.equal(existsSync(markerPath), false);
    } finally {
        rmSync(directory, { force: true, recursive: true });
    }
});

test("rejects malformed selections before launching the child", () => {
    const invalidSelections = [
        { name: "invalid JSON", raw: "not-json", message: /valid JSON/ },
        { name: "non-array JSON", raw: JSON.stringify({ path: "README.md" }), message: /JSON array/ },
        { name: "empty array", raw: "[]", message: /at least one path/ },
        { name: "non-string path", raw: JSON.stringify([42]), message: /must be a string/ },
        { name: "absolute POSIX path", raw: JSON.stringify(["/tmp/README.md"]), message: /safe repository-relative/ },
        { name: "absolute Windows path", raw: JSON.stringify(["C:\\tmp\\README.md"]), message: /safe repository-relative/ },
        { name: "parent traversal", raw: JSON.stringify(["docs/../README.md"]), message: /safe repository-relative/ },
        { name: "NUL byte", raw: JSON.stringify(["docs/README\u0000.md"]), message: /safe repository-relative/ },
        { name: "wrong extension", raw: JSON.stringify(["README.txt"]), message: /safe repository-relative/ },
        { name: "missing file", raw: JSON.stringify(["README.md"]), message: /existing regular/ },
    ];

    for (const selection of invalidSelections) {
        const directory = makeTemporaryDirectory();
        try {
            const { result, capturePath } = runHelper(directory, selection.raw);

            assert.notEqual(result.status, 0, selection.name);
            assert.match(result.stderr, selection.message, selection.name);
            assert.equal(existsSync(capturePath), false, selection.name);
        } finally {
            rmSync(directory, { force: true, recursive: true });
        }
    }
});

// Workflow precondition: deleted-only changes are skipped upstream, so this
// helper receives no empty selection. The helper rejects [] as a fail-closed
// guard if that precondition is ever violated.
for (const rejectionCase of [
    {
        name: "requires the changed-files environment variable",
        message: /VTCODE_CHANGED_MARKDOWN_FILES_JSON is required/,
    },
    {
        name: "rejects an existing directory with a Markdown suffix",
        rawChangedFiles: JSON.stringify(["README.md"]),
        message: /existing regular/,
        setup: (directory) => mkdirSync(join(directory, "README.md")),
    },
    {
        name: "rejects an empty selection before child launch",
        rawChangedFiles: "[]",
        message: /at least one path/,
    },
]) {
    test(rejectionCase.name, () => assertRejectedBeforeChildLaunch(rejectionCase));
}

test("preserves the child exit status", () => {
    const directory = makeTemporaryDirectory();
    try {
        createFixtureFile(directory, "README.md");
        const { result } = runHelper(directory, JSON.stringify(["README.md"]), { exitStatus: 23 });

        assert.equal(result.status, 23);
    } finally {
        rmSync(directory, { force: true, recursive: true });
    }
});

test("rejects a symlink whose real path escapes the checkout", (t) => {
    const directory = makeTemporaryDirectory();
    const outsideDirectory = makeTemporaryDirectory();
    try {
        const outsideFile = join(outsideDirectory, "outside.md");
        writeFileSync(outsideFile, "");
        const linkPath = join(directory, "docs", "link.md");
        mkdirSync(dirname(linkPath), { recursive: true });
        try {
            symlinkSync(outsideFile, linkPath);
        } catch (error) {
            if (skipUnsupportedSymlinkTest(t, error)) {
                return;
            }
            throw error;
        }

        const { result, capturePath } = runHelper(directory, JSON.stringify(["docs/link.md"]));

        assert.notEqual(result.status, 0);
        assert.match(result.stderr, /resolves outside the checkout/);
        assert.equal(existsSync(capturePath), false);
    } finally {
        rmSync(directory, { force: true, recursive: true });
        rmSync(outsideDirectory, { force: true, recursive: true });
    }
});
