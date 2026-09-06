CARGO ?= cargo
PYTHON ?= python3
YAMLLINT ?= yamllint
ACTIONLINT ?= actionlint
UV ?= uv
BUILD_JOBS ?= --jobs 6
NEXTEST_PROFILE ?= default

.DEFAULT_GOAL := check
.NOTPARALLEL:

.PHONY: check check-fmt test-github-actions-validation lint lint-clippy lint-docs \
	lint-policies lint-shell github-actions-lint advisory build typecheck test \
	test-harness check-ast-grep

# Run the complete local release/PR gate in the same sequential order as the
# existing scripts/check.sh checks.
check: check-fmt lint build test test-harness check-ast-grep advisory

check-fmt:
	$(CARGO) fmt --all -- --check

test-github-actions-validation:
	PYTHONPATH=scripts $(UV) run --no-project --with 'PyYAML==6.0.3' $(PYTHON) -m unittest \
		discover -s scripts/tests -p 'test_github_actions_validation*.py'

lint: lint-shell lint-policies github-actions-lint lint-clippy lint-docs

github-actions-lint:
	$(YAMLLINT) --config-file .yamllint.yml .github/workflows
	$(ACTIONLINT)

lint-shell:
	for script in scripts/*.sh scripts/**/*.sh; do \
		[ -f "$$script" ] || continue; \
		bash -n "$$script" || exit $$?; \
		if grep -Eq '^[[:space:]]*nfo([[:space:]]|$$)' "$$script"; then \
			echo "Found truncated 'nfo' command in $$script (should be print_info):" >&2; \
			grep -En '^[[:space:]]*nfo([[:space:]]|$$)' "$$script" >&2; \
			exit 1; \
		fi; \
	done

lint-policies:
	./scripts/check_workflow_security.sh
	./scripts/lint_structured_logging.sh

# Keep the existing warn-mode reports available without making them part of
# the hard lint baseline.
advisory:
	python3 scripts/check_rust_file_length.py --mode warn --max-lines 500
	python3 scripts/check_no_unwrap_expect_prod.py --mode warn --allowlist scripts/zen_allowlist.txt
	python3 scripts/check_zen_allowlist.py --mode warn --allowlist scripts/zen_allowlist.txt
	python3 scripts/check_agent_legibility.py --mode warn

lint-clippy:
	$(CARGO) clippy --locked --workspace --all-targets --all-features $(BUILD_JOBS) -- -D warnings

# Keep the existing documentation-generation gate. The docsrs warning policy
# joins this target with the later lint-configuration layer.
lint-docs:
	$(CARGO) doc --locked --workspace --no-deps $(BUILD_JOBS)

build:
	$(CARGO) build --locked --workspace $(BUILD_JOBS)

typecheck:
	$(CARGO) check --locked --workspace --all-targets --all-features $(BUILD_JOBS)

test:
	$(CARGO) nextest run --locked --workspace --all-features --no-fail-fast --profile $(NEXTEST_PROFILE) $(BUILD_JOBS)

# These focused suites are a separate full-gate step in scripts/check.sh.
test-harness:
	$(CARGO) nextest run --locked -p vtcode-core -E 'binary(/pty_tests/)' $(BUILD_JOBS)
	$(CARGO) nextest run --locked -p vtcode-bash-runner -E 'binary(/pipe_tests/)' $(BUILD_JOBS)
	$(CARGO) nextest run --locked -p vtcode -E 'test(/inline_events/)' $(BUILD_JOBS)

# The legacy full gate treats ast-grep as optional when it is not installed.
check-ast-grep:
	if command -v ast-grep >/dev/null 2>&1; then \
		$(CARGO) run --locked --bin vtcode -- check ast-grep; \
	else \
		echo "ast-grep is not installed; skipping optional repository scan."; \
	fi
