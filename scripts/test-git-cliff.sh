#!/usr/bin/env bash
# shellcheck source-path=SCRIPTDIR

# Deterministic changelog contract checks for git-cliff and both shell emitters.

set -euo pipefail

# Keep Git isolation local even when this test script is sourced.
(
unset GIT_ALTERNATE_OBJECT_DIRECTORIES \
	GIT_COMMON_DIR \
	GIT_CONFIG \
	GIT_CONFIG_COUNT \
	GIT_CONFIG_GLOBAL \
	GIT_CONFIG_NOSYSTEM \
	GIT_CONFIG_PARAMETERS \
	GIT_CONFIG_SYSTEM \
	GIT_DIR \
	GIT_DISCOVERY_ACROSS_FILESYSTEM \
	GIT_EXEC_PATH \
	GIT_GRAFT_FILE \
	GIT_IMPLICIT_WORK_TREE \
	GIT_INDEX_FILE \
	GIT_INTERNAL_SUPER_PREFIX \
	GIT_NO_REPLACE_OBJECTS \
	GIT_OBJECT_DIRECTORY \
	GIT_PREFIX \
	GIT_QUARANTINE_PATH \
	GIT_REPLACE_REF_BASE \
	GIT_SHALLOW_FILE \
	GIT_TEMPLATE_DIR \
	GIT_WORK_TREE
export GIT_CONFIG_GLOBAL=/dev/null
export GIT_CONFIG_NOSYSTEM=1
for git_cliff_variable in "${!GIT_CLIFF@}"; do
	unset "$git_cliff_variable"
done

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if ! command -v git-cliff >/dev/null 2>&1; then
	echo "git-cliff is required" >&2
	exit 1
fi

TEST_ROOT=$(mktemp -d)
trap 'rm -rf "$TEST_ROOT"' EXIT
FIXTURE_REPO="$TEST_ROOT/repository"
mkdir -p "$FIXTURE_REPO"

file_mode() {
	stat -c '%a' "$1" 2>/dev/null || stat -f '%Lp' "$1"
}

source_release_library() {
	case "$1" in
	release.sh)
		# shellcheck source=release.sh
		source "$REPO_ROOT/scripts/release.sh"
		;;
	release-lib.sh)
		# shellcheck source=release-lib.sh
		source "$REPO_ROOT/scripts/release-lib.sh"
		;;
	*)
		echo "unsupported release library: $1" >&2
		return 64
		;;
	esac
}

git -C "$FIXTURE_REPO" init --quiet
git -C "$FIXTURE_REPO" config user.name "Fixture Author"
git -C "$FIXTURE_REPO" config user.email "fixture@example.com"

commit_fixture() {
	local message=$1
	local name=$2
	local email=$3
	local timestamp=$4
	printf '%s\n' "$message" >>"$FIXTURE_REPO/history.txt"
	git -C "$FIXTURE_REPO" add history.txt
	GIT_AUTHOR_NAME="$name" \
		GIT_AUTHOR_EMAIL="$email" \
		GIT_AUTHOR_DATE="$timestamp" \
		GIT_COMMITTER_NAME="$name" \
		GIT_COMMITTER_EMAIL="$email" \
		GIT_COMMITTER_DATE="$timestamp" \
		git -C "$FIXTURE_REPO" commit --quiet -m "$message"
}

commit_fixture "chore: seed fixture" "Fixture Author" "fixture@example.com" \
	"2026-01-01T00:00:00Z"
git -C "$FIXTURE_REPO" tag v1.0.0
commit_fixture "fix: repair beta" "Alice Example" "alice@example.com" \
	"2026-01-02T00:00:00Z"
commit_fixture "docs: explain gamma" "Bob Example" "bob@example.com" \
	"2026-01-03T00:00:00Z"

FIX_HASH_8=$(git -C "$FIXTURE_REPO" rev-parse --short=8 HEAD~1)
DOCS_HASH_8=$(git -C "$FIXTURE_REPO" rev-parse --short=8 HEAD)
FIX_HASH_SHORT=$(git -C "$FIXTURE_REPO" rev-parse --short HEAD~1)
DOCS_HASH_SHORT=$(git -C "$FIXTURE_REPO" rev-parse --short HEAD)

unset GITHUB_TOKEN
(
	cd "$FIXTURE_REPO"
	# The production config enables GitHub metadata; keep this fixture offline.
	git-cliff --offline --config "$REPO_ROOT/cliff.toml" --tag 1.1.0 \
		--output "$TEST_ROOT/git-cliff.md" v1.0.0..HEAD
)

(
	cd "$FIXTURE_REPO"
	source_release_library release.sh
	{
		printf '# Changelog\n\nFixture output.\n\n## 1.1.0 - 2026-01-03\n\n'
		generate_structured_changelog v1.0.0..HEAD 1.1.0
	} >"$TEST_ROOT/release-sh.md"
)

(
	cd "$FIXTURE_REPO"
	source_release_library release-lib.sh
	{
		printf '# Changelog\n\nFixture output.\n\n## 1.1.0 - 2026-01-03\n\n'
		generate_structured_changelog v1.0.0..HEAD 1.1.0
	} >"$TEST_ROOT/release-lib.md"
)

assert_markdown_contract() {
	python3 - "$1" 1.1.0 <<'PY'
import collections
import re
import sys
from pathlib import Path

path = Path(sys.argv[1])
version = sys.argv[2]
text = path.read_text()
headings = []
fence = None
for line_number, line in enumerate(text.splitlines(), 1):
    fence_match = re.match(r"^\s*(`{3,}|~{3,})", line)
    if fence is None:
        heading_match = re.match(r"^(#{1,6})\s+(.+?)\s*$", line)
        if heading_match:
            headings.append(
                (line_number, len(heading_match.group(1)), heading_match.group(2))
            )
    if fence_match:
        marker = fence_match.group(1)[0]
        if fence == marker:
            fence = None
        elif fence is None:
            fence = marker

diagnostic = f"path: {path}\nparsed headings: {headings!r}\n--- rendered Markdown ---\n{text}"
h1 = [heading for heading in headings if heading[1] == 1]
assert [heading[2] for heading in h1] == ["Changelog"], diagnostic
titles = [heading[2].casefold() for heading in headings]
duplicates = [title for title, count in collections.Counter(titles).items() if count > 1]
assert not duplicates, f"duplicate headings: {duplicates!r}\n{diagnostic}"
previous = 0
for line_number, level, _ in headings:
    assert not previous or level <= previous + 1, (
        f"heading jump at line {line_number}: {previous} to {level}\n{diagnostic}"
    )
    previous = level
assert any(
    level == 2 and re.match(rf"^{re.escape(version)} - \d{{4}}-\d{{2}}-\d{{2}}$", title)
    for _, level, title in headings
), f"missing release heading for {version}\n{diagnostic}"
for _, level, title in headings:
    if level >= 3:
        assert title.startswith(f"[{version}] "), (
            f"unqualified level-{level} heading: {title!r}\n{diagnostic}"
        )
PY
}

assert_markdown_contract "$TEST_ROOT/git-cliff.md"
assert_markdown_contract "$TEST_ROOT/release-sh.md"
assert_markdown_contract "$TEST_ROOT/release-lib.md"

python3 - "$TEST_ROOT/git-cliff.md" "$FIX_HASH_8" "$DOCS_HASH_8" <<'PY'
import sys
from pathlib import Path

text = Path(sys.argv[1]).read_text()
fix_hash, docs_hash = sys.argv[2:]
headings = [line for line in text.splitlines() if line.startswith("#")]
rendered = f"rendered headings: {headings!r}\n--- rendered changelog ---\n{text}"
assert "[1.1.0] Bug Fixes" in text, rendered
assert "[1.1.0] Documentation" in text, rendered
assert fix_hash in text, f"missing fix hash {fix_hash!r}\n{rendered}"
assert docs_hash in text, f"missing docs hash {docs_hash!r}\n{rendered}"
assert text.index(fix_hash) < text.index(docs_hash), rendered
PY

python3 - "$TEST_ROOT/release-sh.md" "$FIX_HASH_SHORT" "$DOCS_HASH_SHORT" <<'PY'
import sys
from pathlib import Path

path = Path(sys.argv[1])
text = path.read_text()
fix_hash, docs_hash = sys.argv[2:]
headings = [line for line in text.splitlines() if line.startswith("#")]
diagnostic = f"path: {path}\nrendered headings: {headings!r}\n--- rendered Markdown ---\n{text}"
assert "[1.1.0] Bug Fixes" in text, diagnostic
assert "[1.1.0] Documentation" in text, diagnostic
assert fix_hash in text, f"missing fix hash {fix_hash!r}\n{diagnostic}"
assert docs_hash in text, f"missing docs hash {docs_hash!r}\n{diagnostic}"
assert text.index(fix_hash) < text.index(docs_hash), diagnostic
assert "(@alice)" in text, f"missing Alice username\n{diagnostic}"
assert "(@bob)" in text, f"missing Bob username\n{diagnostic}"
PY

python3 - "$TEST_ROOT/release-lib.md" "$FIX_HASH_SHORT" "$DOCS_HASH_SHORT" <<'PY'
import sys
from pathlib import Path

path = Path(sys.argv[1])
text = path.read_text()
fix_hash, docs_hash = sys.argv[2:]
headings = [line for line in text.splitlines() if line.startswith("#")]
diagnostic = f"path: {path}\nrendered headings: {headings!r}\n--- rendered Markdown ---\n{text}"
assert "[1.1.0] Bug Fixes" in text, diagnostic
assert "[1.1.0] Documentation" in text, diagnostic
assert fix_hash in text, f"missing fix hash {fix_hash!r}\n{diagnostic}"
assert docs_hash in text, f"missing docs hash {docs_hash!r}\n{diagnostic}"
assert "[1.1.0] Contributors" in text, diagnostic
assert "Alice Example" in text, f"missing Alice contributor\n{diagnostic}"
assert "Bob Example" in text, f"missing Bob contributor\n{diagnostic}"
PY

cat >"$TEST_ROOT/insertion-input.md" <<'EOF'
# Changelog

First preamble paragraph.

Second preamble paragraph.

## 1.0.0 - 2026-01-01

- Existing entry.
EOF

SECTION=$'## 1.1.0 - 2026-01-03\n\n### [1.1.0] Fixed\n\n- New entry.'
cp "$TEST_ROOT/insertion-input.md" "$TEST_ROOT/release-sh-insertion.md"
cp "$TEST_ROOT/insertion-input.md" "$TEST_ROOT/release-lib-insertion.md"
chmod 640 "$TEST_ROOT/release-sh-insertion.md" "$TEST_ROOT/release-lib-insertion.md"

(
	source_release_library release.sh
	insert_changelog_section "$TEST_ROOT/release-sh-insertion.md" "$SECTION"
)

[[ $(file_mode "$TEST_ROOT/release-sh-insertion.md") == 640 ]]
(
	source_release_library release-lib.sh
	insert_changelog_section "$TEST_ROOT/release-lib-insertion.md" "$SECTION"
)
[[ $(file_mode "$TEST_ROOT/release-lib-insertion.md") == 640 ]]
for stem in release-sh-insertion release-lib-insertion; do
	if compgen -G "$TEST_ROOT/.$stem.md.section.*" >/dev/null ||
		compgen -G "$TEST_ROOT/.$stem.md.output.*" >/dev/null; then
		echo "successful insertion left temporary files" >&2
		exit 1
	fi
done

python3 - "$TEST_ROOT/insertion-input.md" \
	"$TEST_ROOT/release-sh-insertion.md" "$TEST_ROOT/release-lib-insertion.md" <<'PY'
import sys
from pathlib import Path

source_path = Path(sys.argv[1])
source = source_path.read_text()
for output_name in sys.argv[2:]:
    output_path = Path(output_name)
    output = output_path.read_text()
    diagnostic = (
        f"source path: {source_path}\noutput path: {output_path}"
        f"\n--- source Markdown ---\n{source}"
        f"\n--- output Markdown ---\n{output}"
    )
    preamble = source.split("## 1.0.0", 1)[0]
    assert output.startswith(preamble), diagnostic
    assert "## 1.1.0" in output, f"missing new release heading\n{diagnostic}"
    assert "## 1.0.0" in output, f"missing existing release heading\n{diagnostic}"
    assert output.index("## 1.1.0") < output.index("## 1.0.0"), diagnostic
    assert output.count("# Changelog") == 1, diagnostic
    assert "- Existing entry." in output, f"missing existing entry\n{diagnostic}"
    assert "- New entry." in output, f"missing new entry\n{diagnostic}"
PY

assert_insertion_failure_preserves_original() {
	local library=$1
	local failed_command=$2
	local expected_status=$3
	local stem=${library%.sh}-${failed_command}-failure
	local changelog="$TEST_ROOT/$stem.md"
	local pristine="$TEST_ROOT/$stem.expected"

	cp "$TEST_ROOT/insertion-input.md" "$changelog"
	chmod 640 "$changelog"
	cp -p "$changelog" "$pristine"
	local shim_dir="$TEST_ROOT/$stem-bin"
	mkdir -p "$shim_dir"
	printf '#!/bin/sh\nexit %s\n' "$expected_status" >"$shim_dir/$failed_command"
	chmod +x "$shim_dir/$failed_command"
	(
		source_release_library "$library"
		PATH="$shim_dir:$PATH"
		export PATH
		local status
		if insert_changelog_section "$changelog" "$SECTION"; then
			echo "insertion unexpectedly succeeded after $failed_command failure" >&2
			exit 1
		else
			status=$?
		fi
		if [[ $status -ne $expected_status ]]; then
			echo "$library returned $status after $failed_command failure; expected $expected_status" >&2
			exit 1
		fi
	)
	cmp -s "$pristine" "$changelog"
	[[ $(file_mode "$changelog") == 640 ]]
	if compgen -G "$TEST_ROOT/.$stem.md.section.*" >/dev/null ||
		compgen -G "$TEST_ROOT/.$stem.md.output.*" >/dev/null; then
		echo "insertion left temporary files after $failed_command failure" >&2
		return 1
	fi
}

for library in release.sh release-lib.sh; do
	assert_insertion_failure_preserves_original "$library" awk 71
	assert_insertion_failure_preserves_original "$library" mv 72
done

cat >"$TEST_ROOT/historical-headings.md" <<'EOF'
# Changelog

## [Version 0.9.0] - 2025-01-01

# v0.8.0 - 2024-01-01

## scode-v0.7.0 - 2023-01-01
EOF

for library in release.sh release-lib.sh; do
	(
		source_release_library "$library"
		changelog_contains_version "$TEST_ROOT/historical-headings.md" 0.9.0
		changelog_contains_version "$TEST_ROOT/historical-headings.md" v0.8.0
		changelog_contains_version "$TEST_ROOT/historical-headings.md" 0.7.0
		if changelog_contains_version "$TEST_ROOT/historical-headings.md" 9.9.9; then
			echo "unexpected duplicate-version match in $library" >&2
			exit 1
		fi
	)
done

echo "Changelog generator contracts passed."
)
