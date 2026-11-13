#!/usr/bin/env bash

set -euo pipefail
umask 077

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALLERS_DIR="${INSTALLERS_DIR:-$script_dir}"
BASH_INSTALLER="$INSTALLERS_DIR/install.sh"
POWERSHELL_INSTALLER="$INSTALLERS_DIR/install.ps1"
[[ -f "$BASH_INSTALLER" && -f "$POWERSHELL_INSTALLER" ]] || {
    printf 'Installer files not found under INSTALLERS_DIR=%s\n' "$INSTALLERS_DIR" >&2
    exit 1
}

test_root="$(mktemp -d)"
test_home="$test_root/home"
help_home="$test_root/help home"
powershell_home="$test_root/powershell home"
test_tmp="$test_root/tmp with spaces"
mock_bin="$test_root/mock bin"
artefact="$test_root/diapason-test-binary"
global_sentinel=""
original_path="$PATH"

cleanup_test_files() {
    local status=$?
    if (( status == 0 )); then
        rm -rf -- "$test_root"
    else
        printf 'Installer test artefacts retained at %s\n' "$test_root" >&2
    fi
    if [[ -n "$global_sentinel" ]]; then
        rm -f -- "$global_sentinel"
    fi
    return "$status"
}
trap cleanup_test_files EXIT

mkdir -p "$test_home" "$help_home" "$powershell_home" "$test_tmp" "$mock_bin"
global_sentinel="$(mktemp /tmp/diapason-installer-sentinel.XXXXXX)"
printf 'preserve this unrelated temporary file\n' > "$global_sentinel"
printf 'preserve this PowerShell temporary file\n' > "$test_tmp/diapason-unrelated-sentinel"

cat > "$artefact" <<'EOF'
#!/bin/sh
if [ "${1:-}" = "--version" ]; then
    printf '%s\n' 'diapason-test-version'
    exit 0
fi
exit 0
EOF
chmod +x "$artefact"

cat > "$mock_bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
cargo_root=""
while (($#)); do
    if [[ "$1" == "--root" ]]; then
        cargo_root="$2"
        shift 2
    else
        shift
    fi
done
[[ -n "$cargo_root" ]] || { printf 'mock Cargo received no --root\n' >&2; exit 90; }
mkdir -p "$cargo_root"
printf '%s\n' "$cargo_root" > "$CARGO_TEST_ROOT_RECORD"
case "$CARGO_TEST_MODE" in
    failure) exit 42 ;;
    missing-artefact) exit 0 ;;
    success)
        mkdir -p "$cargo_root/bin"
        cp "$CARGO_TEST_ARTEFACT" "$cargo_root/bin/diapason"
        cp "$CARGO_TEST_ARTEFACT" "$cargo_root/bin/diapason.exe"
        chmod +x "$cargo_root/bin/diapason" "$cargo_root/bin/diapason.exe"
        ;;
    *) printf 'unknown mock Cargo mode: %s\n' "$CARGO_TEST_MODE" >&2; exit 91 ;;
esac
EOF
chmod +x "$mock_bin/cargo"

fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

assert_artefact_matches() {
    cmp -s "$artefact" "$1" || fail "artefact bytes differ at $1"
}

assert_version_probe() {
    grep -Fq 'Version check passed: diapason-test-version' "$1" \
        || fail "version check did not report the installed binary at $1"
    if grep -Eq 'command not found|No such file or directory|not recognized' "$1"; then
        fail "installer emitted command lookup errors at $1"
    fi
}

assert_cargo_root_removed() {
    local record_file="$1"
    [[ -f "$record_file" ]] || fail 'mock Cargo did not record its staging root'
    local cargo_root
    cargo_root="$(<"$record_file")"
    case "$cargo_root" in
        "$test_tmp"/*) ;;
        *) fail "Cargo staging root escaped isolated temp directory: $cargo_root" ;;
    esac
    [[ ! -e "$cargo_root" ]] || fail "Cargo staging root was not removed: $cargo_root"
}

run_bash_installer() {
    local case_dir="$1" mode="$2" target_dir="$3"
    shift 3
    mkdir -p "$case_dir"
    env HOME="$test_home" TMPDIR="$test_tmp" TEMP="$test_tmp" \
        PATH="$target_dir:$mock_bin:$original_path" INSTALL_DIR= DIAPASON_BIN= \
        CARGO_TEST_MODE="$mode" CARGO_TEST_ROOT_RECORD="$case_dir/cargo-root" \
        CARGO_TEST_ARTEFACT="$artefact" \
        bash "$BASH_INSTALLER" "$@" > "$case_dir/output.log" 2>&1
}

test_bash_help() {
    local case_dir="$test_root/bash-help"
    mkdir -p "$case_dir"
    env HOME="$help_home" TMPDIR="$test_tmp" TEMP="$test_tmp" \
        PATH="$mock_bin:$original_path" INSTALL_DIR= DIAPASON_BIN= \
        bash "$BASH_INSTALLER" --help > "$case_dir/output.log" 2>&1
    [[ ! -e "$help_home/.local/bin" ]] || fail 'Bash --help created the default install directory'
}

test_bash_success() {
    local case_dir="$1" mode="$2" target_dir="$3" expected="$4"
    shift 4
    run_bash_installer "$case_dir" "$mode" "$target_dir" "$@"
    assert_artefact_matches "$expected"
    assert_cargo_root_removed "$case_dir/cargo-root"
    if [[ "$expected" == "$target_dir/diapason" ]]; then
        assert_version_probe "$case_dir/output.log"
    fi
}

test_bash_success "$test_root/bash-custom" success "$test_root/custom tools" \
    "$test_root/custom tools/diapason" --dir "$test_root/custom tools" --source-build --without-search-tools
test_bash_help
test_bash_success "$test_root/bash-default" success "$test_home/.local/bin" \
    "$test_home/.local/bin/diapason" --source-build --without-search-tools

for mode in failure missing-artefact; do
    case_dir="$test_root/bash-$mode"
    target_dir="$test_root/bash $mode target"
    if run_bash_installer "$case_dir" "$mode" "$target_dir" \
        --dir "$target_dir" --source-build --without-search-tools; then
        fail "Bash accepted Cargo mode $mode"
    fi
    [[ ! -e "$target_dir/diapason" ]] || fail "Bash installed an artefact after Cargo mode $mode"
    assert_cargo_root_removed "$case_dir/cargo-root"
done

local_case="$test_root/bash-local-binary"
local_target="$test_root/local binary target"
mkdir -p "$local_case"
run_bash_installer "$local_case" failure "$local_target" \
    --dir "$local_target" --binary "$artefact" --without-search-tools
assert_artefact_matches "$local_target/diapason"
[[ ! -e "$local_case/cargo-root" ]] || fail 'Bash invoked Cargo for a local binary install'
[[ -f "$global_sentinel" ]] || fail 'Bash removed an unrelated /tmp/diapason-* file'
printf 'Bash installer source-build checks passed.\n'

if command -v pwsh >/dev/null 2>&1; then
    POWERSHELL_BIN="$(command -v pwsh)"
    run_powershell_installer() {
        local case_dir="$1" mode="$2" target_dir="$3"
        shift 3
        mkdir -p "$case_dir"
        env HOME="$powershell_home" USERPROFILE="$powershell_home" TMPDIR="$test_tmp" \
            TEMP="$test_tmp" TMP="$test_tmp" PATH="$mock_bin:$original_path;$target_dir" \
            CARGO_TEST_MODE="$mode" CARGO_TEST_ROOT_RECORD="$case_dir/cargo-root" \
            CARGO_TEST_ARTEFACT="$artefact" \
            "$POWERSHELL_BIN" -NoLogo -NoProfile -File "$POWERSHELL_INSTALLER" "$@" \
            > "$case_dir/output.log" 2>&1
    }

    test_powershell_success() {
        local case_dir="$1" mode="$2" target_dir="$3" expected="$4"
        shift 4
        run_powershell_installer "$case_dir" "$mode" "$target_dir" "$@"
        assert_artefact_matches "$expected"
        assert_cargo_root_removed "$case_dir/cargo-root"
        if [[ "$expected" == "$target_dir/diapason.exe" ]]; then
            assert_version_probe "$case_dir/output.log"
        fi
    }

    test_powershell_success "$test_root/powershell-custom" success \
        "$test_root/powershell custom tools" "$test_root/powershell custom tools/diapason.exe" \
        -InstallDir "$test_root/powershell custom tools" -SourceBuild
    test_powershell_success "$test_root/powershell-default" success "$powershell_home/.local/bin" \
        "$powershell_home/.local/bin/diapason.exe" -SourceBuild

    for mode in failure missing-artefact; do
        case_dir="$test_root/powershell-$mode"
        target_dir="$test_root/powershell $mode target"
        if run_powershell_installer "$case_dir" "$mode" "$target_dir" \
            -InstallDir "$target_dir" -SourceBuild; then
            fail "PowerShell accepted Cargo mode $mode"
        fi
        [[ ! -e "$target_dir/diapason.exe" ]] || fail "PowerShell installed an artefact after Cargo mode $mode"
        assert_cargo_root_removed "$case_dir/cargo-root"
    done

    local_case="$test_root/powershell-local-binary"
    local_target="$test_root/powershell local binary target"
    mkdir -p "$local_case"
    run_powershell_installer "$local_case" failure "$local_target" \
        -InstallDir "$local_target" -BinaryPath "$artefact"
    assert_artefact_matches "$local_target/diapason.exe"
    [[ ! -e "$local_case/cargo-root" ]] || fail 'PowerShell invoked Cargo for a local binary install'
    [[ -f "$test_tmp/diapason-unrelated-sentinel" ]] \
        || fail 'PowerShell removed an unrelated temporary file'
    printf 'PowerShell installer source-build checks passed.\n'
else
    printf 'pwsh not found; skipping PowerShell installer checks.\n'
fi
