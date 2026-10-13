# Debugging Plan: Hosted shell-parser property test SIGSEGV

**Generated**: 2026-10-13  
**Issue ID**: PR #11, CI run 38393400372  
**Severity**: High — blocks merge until the required hosted check passes  
**Falsification sub-agent**: `alchemist`  
**Planning agent boundary**: This document was prepared by the planning agent.
Falsification must be executed by the named sub-agent, not by the planning
agent.

## Problem Statement

On PR #11 head `fc8872bae85c782d831aaae204c19c0a5a6a9253`, hosted
`cargo nextest` aborted
`vtcode-core::core_fuzz_props::fuzz_shell_parser_never_panics` with SIGSEGV on
all three CI retries. The run reported 7,319 passed, one failed and 16 skipped.
The property generates non-control Unicode strings up to 512 characters and
calls `parse_shell_commands`. The preceding local `make test` passed, but it
uses the default Cargo profile; CI explicitly uses `--cargo-profile ci`.

## Context Summary

| Aspect              | Details                                                                                                                                                                                                                                             |
| ------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| First observed      | CI run 38393400372, 2026-10-13; failed head `fc8872bae85c782d831aaae204c19c0a5a6a9253`                                                                                                                                                              |
| Reproduction rate   | One hosted run; the test failed on all three retries. A local default-profile suite passed once.                                                                                                                                                    |
| Affected components | `vtcode-core/tests/core_fuzz_props.rs`; `vtcode-safety::command_safety::shell_parser::parse_shell_commands`; tree-sitter Bash parser                                                                                                                |
| Recent changes      | The property test and parser were unchanged by PR #11. The PR changes tool argument validation and Cargo.lock dependencies. CI uses the `ci` profile (`debug = 0`, `overflow-checks = false`); local `make test` defaults to the `default` profile. |

### Error Artefacts

```plaintext
TRY 3 SEGV [0.187s] vtcode-core::core_fuzz_props::fuzz_shell_parser_never_panics
proptest: FileFailurePersistence::SourceParallel set, but failed to find lib.rs or main.rs
(test aborted with signal 11: SIGSEGV)
Summary: 7320 tests run: 7319 passed, 1 failed, 16 skipped
```

### Information Gaps

- CI did not preserve the Proptest seed, generated script, core dump or native
  backtrace.
- The warning about source-parallel failure persistence is present, but there
  is no evidence that it caused the signal.
- A fixed-seed local batch was compared under the CI and development Cargo
  profiles; both passed.

______________________________________________________________________

## Hypotheses

### H1: A generated script exposes a parser crash

**Claim**: A script from the property's `\\PC{0,512}` input space can make the
tree-sitter Bash parser crash, independently of the Cargo profile.

**Plausibility**: Medium — the signal is repeatable across CI retries and the
property exercises a native parser, but the parser and property test were not
changed in this PR.

**Prediction**: A fixed-seed batch with the same generated inputs will crash in
both the `ci` and `dev` Cargo profiles, or a captured fault will point into the
parser path.

#### H1 Falsification Plan

| Step | Action                                                                                                                                                                                             | Expected Negative Result                                                                                      |
| ---- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------- |
| 1    | Run only `fuzz_shell_parser_never_panics` with `PROPTEST_RNG_SEED=20261013` and `PROPTEST_CASES=4096`, first with `--profile ci --cargo-profile ci`, then with `--profile ci --cargo-profile dev`. | Both runs complete without a signal, which argues against a common generated-input crash in the tested range. |
| 2    | If either run crashes, capture the failing process backtrace or core and determine whether the fault is in tree-sitter parsing before proposing a code change.                                     | A fault outside the parser path falsifies this parser-input claim.                                            |

**Tooling**: `cargo nextest` focused on `vtcode-core/tests/core_fuzz_props.rs`,
`PROPTEST_RNG_SEED`, `PROPTEST_CASES`, and a native backtrace tool if available.

**Confidence on falsification**: Moderate for common random-input failures;
without the hosted seed, passing fixed-seed batches cannot rule out a rare
script shape.

______________________________________________________________________

### H2: The CI Cargo profile or runner environment causes the crash

**Claim**: The `ci` profile (`debug = 0`, `overflow-checks = false`) or its
runner environment changes the focused test's behavior; the same generated
inputs pass under the default development profile.

**Plausibility**: Medium — the local pass used `make test`'s default profile,
while hosted CI explicitly builds with the `ci` profile and fails in under a
quarter-second on each retry.

**Prediction**: With a fixed Proptest seed and case count, the `ci` profile
fails while the `dev` profile passes.

#### H2 Falsification Plan

| Step | Action                                                                                                                                                                                        | Expected Negative Result                                                                             |
| ---- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| 1    | Compare the same isolated batch (`PROPTEST_RNG_SEED=20261013`, `PROPTEST_CASES=4096`) under `--profile ci --cargo-profile ci` and `--profile ci --cargo-profile dev`; preserve runner limits. | Both profiles have the same outcome, falsifying a profile-specific explanation for that input batch. |

**Tooling**: Run these two commands sequentially, with the same environment for
both:

```bash
PROPTEST_RNG_SEED=20261013 PROPTEST_CASES=4096 cargo nextest run --locked \
  -p vtcode-core --test core_fuzz_props \
  -E 'test(fuzz_shell_parser_never_panics)' --profile ci --cargo-profile ci
PROPTEST_RNG_SEED=20261013 PROPTEST_CASES=4096 cargo nextest run --locked \
  -p vtcode-core --test core_fuzz_props \
  -E 'test(fuzz_shell_parser_never_panics)' --profile ci --cargo-profile dev
```

Do not change parser code or Proptest persistence configuration.

**Confidence on falsification**: Moderate — it isolates Cargo profile effects
for identical inputs, but a different GitHub runner kernel or native library
could still matter.

### Experiment Results

The same-seed experiment ran at HEAD `fc8872bae85c782d831aaae204c19c0a5a6a9253`
with `PROPTEST_RNG_SEED=20261013` and `PROPTEST_CASES=4096`:

| Cargo profile | Result                                                |
| ------------- | ----------------------------------------------------- |
| `ci`          | Exit 0; one test passed, 12 skipped; test time 1.159s |
| `dev`         | Exit 0; one test passed, 12 skipped; test time 1.223s |

The profile-specific prediction is falsified for this input batch. The batch
also found no parser crash in either profile. It did not reproduce the hosted
random input or establish why all three hosted retries received SIGSEGV. No
parser or test behavior change is justified by this evidence.

Captured output: `/tmp/pr11-fuzz-profile-ci-20261013.out` and
`/tmp/pr11-fuzz-profile-dev-20261013.out`.

______________________________________________________________________

## Recommended Execution Order

1. **Completed: H2** — the same-seed profile comparison passed under both
   profiles, falsifying the profile-specific prediction for that batch.
2. **On recurrence: H1** — preserve the failing CI seed/input and native
   backtrace or core before changing parser code or test behavior.

## Termination Criteria

- **Root cause identified**: A captured failing input and native backtrace
  establish the parser or another faulting component.
- **Current status**: The profile-specific hypothesis is falsified for the
  fixed batch. The hosted failure remains unclassified until a run passes or a
  recurrence provides the seed/input and fault location.
- **Escalation trigger**: The hosted test fails again; capture the failing seed
  or input and a native backtrace before changing parser or test behavior.

## Notes for Executing Agent

Run only the supplied focused experiment, one Cargo command at a time, using
the shared Cargo cache and normal compiler admission. Do not run full
repository gates, edit tracked files, change global test settings, or suppress
the test. The planning agent is not the execution agent; return a verdict of
falsified, not-falsified, or inconclusive with the exact seed, profile,
command, and result.
