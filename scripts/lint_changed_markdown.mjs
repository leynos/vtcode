#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { realpathSync, statSync } from "node:fs";
import path from "node:path";

const CHANGED_FILES_ENV = "VTCODE_CHANGED_MARKDOWN_FILES_JSON";
const COMMAND_ENV = "VTCODE_MARKDOWNLINT_COMMAND";
const DEFAULT_COMMAND = "npx";
const MARKDOWNLINT_PACKAGE = "markdownlint-cli2@0.23.2";

function isAbsolutePath(candidate) {
    return (
        path.posix.isAbsolute(candidate) ||
        path.win32.isAbsolute(candidate) ||
        // A drive-relative Windows path can still resolve outside the checkout.
        /^[A-Za-z]:/.test(candidate)
    );
}

function parseChangedFiles(rawValue) {
    if (rawValue === undefined) {
        throw new Error(`${CHANGED_FILES_ENV} is required`);
    }

    const changedFiles = parseChangedFilesJson(rawValue);
    validateChangedFileList(changedFiles);
    return changedFiles;
}

function parseChangedFilesJson(rawValue) {
    let changedFiles;
    try {
        changedFiles = JSON.parse(rawValue);
    } catch {
        throw new Error(`${CHANGED_FILES_ENV} must contain valid JSON`);
    }

    if (!Array.isArray(changedFiles)) {
        throw new Error(`${CHANGED_FILES_ENV} must be a JSON array of paths`);
    }

    return changedFiles;
}

function validateChangedFileList(changedFiles) {
    if (changedFiles.length === 0) {
        throw new Error(`${CHANGED_FILES_ENV} must contain at least one path`);
    }

    for (const [index, candidate] of changedFiles.entries()) {
        validateChangedFileCandidate(candidate, index);
    }
}

function validateChangedFileCandidate(candidate, index) {
    if (typeof candidate !== "string") {
        throw new Error(`${CHANGED_FILES_ENV}[${index}] must be a string`);
    }

    if (isUnsafeChangedFilePath(candidate)) {
        throw new Error(
            `${CHANGED_FILES_ENV}[${index}] must be a safe repository-relative .md path`,
        );
    }
}

function isUnsafeChangedFilePath(candidate) {
    if (candidate.length === 0) {
        return true;
    }
    if (candidate.includes("\0")) {
        return true;
    }
    if (isAbsolutePath(candidate)) {
        return true;
    }
    if (candidate.split(/[\\/]/).includes("..")) {
        return true;
    }
    return !candidate.endsWith(".md");
}

function isWithinCheckoutRoot(checkoutRoot, candidatePath) {
    const relativePath = path.relative(checkoutRoot, candidatePath);
    return (
        relativePath !== ".." &&
        !relativePath.startsWith(`..${path.sep}`) &&
        !path.isAbsolute(relativePath)
    );
}

function validateExistingFiles(changedFiles, checkoutRoot) {
    for (const [index, candidate] of changedFiles.entries()) {
        const candidatePath = path.resolve(checkoutRoot, candidate);
        let realPath;
        try {
            realPath = realpathSync(candidatePath);
        } catch {
            throw new Error(
                `${CHANGED_FILES_ENV}[${index}] must name an existing regular .md file inside the checkout`,
            );
        }

        if (!isWithinCheckoutRoot(checkoutRoot, realPath)) {
            throw new Error(
                `${CHANGED_FILES_ENV}[${index}] resolves outside the checkout`,
            );
        }

        try {
            if (!statSync(realPath).isFile()) {
                throw new Error("not a regular file");
            }
        } catch {
            throw new Error(
                `${CHANGED_FILES_ENV}[${index}] must name an existing regular .md file inside the checkout`,
            );
        }
    }
}

function main() {
    let changedFiles;
    try {
        changedFiles = parseChangedFiles(process.env[CHANGED_FILES_ENV]);
    } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        process.stderr.write(`lint_changed_markdown: ${message}\n`);
        return 1;
    }

    let checkoutRoot;
    try {
        checkoutRoot = realpathSync(process.cwd());
        validateExistingFiles(changedFiles, checkoutRoot);
    } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        process.stderr.write(`lint_changed_markdown: ${message}\n`);
        return 1;
    }

    const command = process.env[COMMAND_ENV] || DEFAULT_COMMAND;
    const args = [
        "--yes",
        "--package",
        MARKDOWNLINT_PACKAGE,
        "markdownlint-cli2",
        "--",
        ...changedFiles,
    ];
    const result = spawnSync(command, args, {
        cwd: process.cwd(),
        shell: false,
        stdio: "inherit",
    });

    if (result.error) {
        process.stderr.write(
            `lint_changed_markdown: failed to launch ${command}: ${result.error.message}\n`,
        );
        return 1;
    }

    return typeof result.status === "number" ? result.status : 1;
}

process.exitCode = main();
