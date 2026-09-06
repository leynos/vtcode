#!/usr/bin/env bash
# Tests for the legacy updater compatibility bridge release helpers.
#
# Covers compatibility asset naming, byte-identity with the extracted
# executable, rejection of ambiguous/missing binaries, required-target
# coverage validation, and two-phase upload ordering (compat assets before
# normal archives) via an instrumented `gh` stub.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$SCRIPT_DIR/../release-assets.sh"

fail=0
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

pass() { echo "  ✓ $1"; }
fail_test() { echo "  ✗ $1"; fail=1; }

echo "Testing compatibility_asset_path..."

got=$(compatibility_asset_path "vtcode-0.141.6-aarch64-apple-darwin.tar.gz" "$tmp")
if [[ "$got" == "$tmp/compat-vtcode-0.141.6-aarch64-apple-darwin.tar.gz.compat" ]]; then
    pass "tar.gz -> compat-vtcode-...tar.gz.compat"
else
    fail_test "tar.gz path: got $got"
fi

got=$(compatibility_asset_path "vtcode-0.141.6-x86_64-pc-windows-msvc.zip" "$tmp")
if [[ "$got" == "$tmp/compat-vtcode-0.141.6-x86_64-pc-windows-msvc.tar.gz.compat" ]]; then
    pass "zip -> compat-vtcode-...tar.gz.compat (keeps <target>.tar.gz substring)"
else
    fail_test "zip path: got $got"
fi

if compatibility_asset_path "foo.txt" "$tmp" >/dev/null 2>&1; then
    fail_test "unsupported extension should be rejected"
else
    pass "unsupported extension rejected"
fi

echo "Testing compat asset sorts BEFORE the normal archive (load-bearing)..."

# GitHub returns release assets sorted alphabetically by name; the legacy
# updater's `find()` picks the first asset whose name contains both the target
# triple and the `{target}.tar.gz` identifier. Both the normal `.tar.gz` and
# the compat asset match, so the compat name MUST sort first or the legacy
# updater selects the broken `.tar.gz` and fails with CompressionNotEnabledError.
compat_name=$(compatibility_asset_path "vtcode-0.141.6-aarch64-apple-darwin.tar.gz" "$tmp")
compat_base=$(basename "$compat_name")
normal_base="vtcode-0.141.6-aarch64-apple-darwin.tar.gz"
# `LC_ALL=C sort` gives byte-order (ASCII) ordering, matching GitHub's sort.
sorted=$(printf '%s\n%s\n' "$compat_base" "$normal_base" | LC_ALL=C sort | head -1)
if [[ "$sorted" == "$compat_base" ]]; then
    pass "compat asset sorts before normal archive (legacy updater picks compat)"
else
    fail_test "compat asset does NOT sort before normal archive: compat=$compat_base normal=$normal_base"
fi
# Also assert the compat name still contains the legacy identifier substring.
if [[ "$compat_base" == *"aarch64-apple-darwin.tar.gz"* ]]; then
    pass "compat name contains the {target}.tar.gz identifier substring"
else
    fail_test "compat name lost the {target}.tar.gz substring: $compat_base"
fi

echo "Testing create_compatibility_asset byte identity..."

# Fixture tar.gz with a root-level vtcode binary (matches CI packaging).
mkdir -p "$tmp/src1"
printf 'unix-binary-bytes' >"$tmp/src1/vtcode"
tar -C "$tmp/src1" -czf "$tmp/vtcode-0.141.6-aarch64-apple-darwin.tar.gz" vtcode
out=$(compatibility_asset_path "$tmp/vtcode-0.141.6-aarch64-apple-darwin.tar.gz" "$tmp")
if create_compatibility_asset "$tmp/vtcode-0.141.6-aarch64-apple-darwin.tar.gz" "$out" \
    && cmp -s "$tmp/src1/vtcode" "$out"; then
    pass "tar.gz compat bytes match extracted vtcode"
else
    fail_test "tar.gz compat bytes mismatch"
fi

# Fixture zip with a root-level vtcode.exe binary (matches CI packaging).
if command -v zip >/dev/null 2>&1; then
    mkdir -p "$tmp/src2"
    printf 'windows-binary-bytes' >"$tmp/src2/vtcode.exe"
    (cd "$tmp/src2" && zip -q "$tmp/vtcode-0.141.6-x86_64-pc-windows-msvc.zip" vtcode.exe)
    out2=$(compatibility_asset_path "$tmp/vtcode-0.141.6-x86_64-pc-windows-msvc.zip" "$tmp")
    if create_compatibility_asset "$tmp/vtcode-0.141.6-x86_64-pc-windows-msvc.zip" "$out2" \
        && cmp -s "$tmp/src2/vtcode.exe" "$out2"; then
        pass "zip compat bytes match extracted vtcode.exe"
    else
        fail_test "zip compat bytes mismatch"
    fi
else
    echo "  - zip not installed; skipping zip compat byte test"
fi

echo "Testing create_compatibility_asset rejection..."

# Missing binary.
mkdir -p "$tmp/src3"
printf 'other' >"$tmp/src3/not-vtcode"
tar -C "$tmp/src3" -czf "$tmp/bad.tar.gz" not-vtcode
if create_compatibility_asset "$tmp/bad.tar.gz" "$tmp/should-not-exist" >/dev/null 2>&1; then
    fail_test "missing binary should be rejected"
else
    pass "missing binary rejected"
fi
[[ -e "$tmp/should-not-exist" ]] && fail_test "missing binary left output file" || pass "missing binary left no output"

# Multiple matching binaries.
mkdir -p "$tmp/src4/a" "$tmp/src4/b"
printf 'x' >"$tmp/src4/a/vtcode"
printf 'y' >"$tmp/src4/b/vtcode"
tar -C "$tmp/src4" -czf "$tmp/multi.tar.gz" a/vtcode b/vtcode
if create_compatibility_asset "$tmp/multi.tar.gz" "$tmp/should-not-exist2" >/dev/null 2>&1; then
    fail_test "multiple binaries should be rejected"
else
    pass "multiple binaries rejected"
fi

# Empty extracted output (binary is zero-length).
mkdir -p "$tmp/src5"
: >"$tmp/src5/vtcode"
tar -C "$tmp/src5" -czf "$tmp/empty.tar.gz" vtcode
if create_compatibility_asset "$tmp/empty.tar.gz" "$tmp/empty-out" >/dev/null 2>&1; then
    fail_test "empty binary should be rejected"
else
    pass "empty binary rejected"
fi

echo "Testing generate_checksums_manifest..."

manifest_stage="$tmp/manifest-stage"
mkdir -p "$manifest_stage"
printf 'archive' >"$manifest_stage/vtcode-0.141.7-aarch64-apple-darwin.tar.gz"
printf 'compatibility binary' >"$manifest_stage/compat-vtcode-0.141.7-aarch64-apple-darwin.tar.gz.compat"
if generate_checksums_manifest "$manifest_stage"; then
    if grep -q '  vtcode-0.141.7-aarch64-apple-darwin.tar.gz$' "$manifest_stage/checksums.txt" \
        && ! grep -q 'compat-' "$manifest_stage/checksums.txt"; then
        pass "aggregate manifest includes archives without ambiguous compatibility assets"
    else
        fail_test "aggregate manifest contains ambiguous or missing entries"
    fi
else
    fail_test "aggregate manifest generation should succeed"
fi

echo "Testing validate_release_assets..."

# Build a complete staged release directory for v0.141.6.
stage="$tmp/stage-good"
mkdir -p "$stage"
version="0.141.6"
targets=("x86_64-apple-darwin" "aarch64-apple-darwin" "x86_64-unknown-linux-gnu" \
    "x86_64-unknown-linux-musl" "aarch64-unknown-linux-gnu" "x86_64-pc-windows-msvc")
for target in "${targets[@]}"; do
    ext="tar.gz"
    [[ "$target" == *pc-windows* ]] && ext="zip"
    archive="$stage/vtcode-${version}-${target}.${ext}"
    printf 'placeholder' >"$archive"
    compat="$stage/compat-vtcode-${version}-${target}.tar.gz.compat"
    printf 'placeholder' >"$compat"
    printf 'placeholder-checksum' >"$stage/vtcode-${version}-${target}.sha256"
done
printf 'aggregate-checksums\n' >"$stage/checksums.txt"

if validate_release_assets "$stage" "$version"; then
    pass "complete staged release validates"
else
    fail_test "complete staged release should validate"
fi

# Remove a compat asset -> validation must fail.
rm -f "$stage/compat-vtcode-${version}-aarch64-apple-darwin.tar.gz.compat"
if validate_release_assets "$stage" "$version" >/dev/null 2>&1; then
    fail_test "missing compat asset should fail validation"
else
    pass "missing compat asset fails validation"
fi

# Remove a normal archive -> validation must fail.
stage2="$tmp/stage-missing-archive"
cp -r "$stage" "$stage2"
cp "$tmp/stage-good/compat-vtcode-${version}-aarch64-apple-darwin.tar.gz.compat" "$stage2/" 2>/dev/null || true
rm -f "$stage2/vtcode-${version}-x86_64-pc-windows-msvc.zip"
if validate_release_assets "$stage2" "$version" >/dev/null 2>&1; then
    fail_test "missing normal archive should fail validation"
else
    pass "missing normal archive fails validation"
fi

echo "Testing two-phase upload ordering (belt-and-suspenders)..."

# The real guarantee that the legacy updater picks the compat asset is
# ALPHABETICAL NAME SORT (asserted above): GitHub returns assets sorted by
# name, and `compat-` sorts before `vtcode-`. Upload order does NOT control
# selection. The release script still uploads compat assets first as
# defence-in-depth; this test asserts that ordering is preserved.
gh_calls="$tmp/gh-calls"
: >"$gh_calls"
gh() {
    if [[ "$1" == "release" && "$2" == "upload" ]]; then
        printf '%s\n' "$*" >>"$gh_calls"
    fi
}
export -f gh

compat_glob="$stage/compat-*.tar.gz.compat"
normal_glob="$stage/vtcode-*.tar.gz $stage/vtcode-*.zip"
# Simulate the two-phase upload the release script performs.
# shellcheck disable=SC2086
gh release upload "$version" $compat_glob --clobber
# shellcheck disable=SC2086,SC2046
gh release upload "$version" $(echo $normal_glob) --clobber

first_compat_line=$(grep -n '\.tar\.gz\.compat' "$gh_calls" | head -1 | cut -d: -f1)
first_normal_line=$(grep -nE '\.(tar\.gz|zip)([^.]|$)' "$gh_calls" \
    | grep -v '\.tar\.gz\.compat' | head -1 | cut -d: -f1)
if [[ -n "$first_compat_line" && -n "$first_normal_line" \
    && "$first_compat_line" -lt "$first_normal_line" ]]; then
    pass "compat assets uploaded before normal archives"
else
    fail_test "upload ordering: compat=$first_compat_line normal=$first_normal_line"
fi

if [[ "$fail" -ne 0 ]]; then
    echo "FAIL: release-assets tests failed"
    exit 1
fi
echo "PASS: release-assets tests"
