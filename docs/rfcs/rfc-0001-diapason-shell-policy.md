# RFC 0001: Diapason shell analysis and unified policy

## Preamble

- **RFC number:** 0001
- **Status:** Proposed
- **Created:** 2026-11-03
- **Audience:** Runtime, safety, and configuration maintainers; policy authors.
- **Scope:** Replace Bash analysis and competing authorization mechanisms in
  vtcode. Diapason is the proposed fork name, not an implemented rename.
- **Companions:** [Technical design](../design/diapason-shell-policy-design.md)
  and [GIST roadmap](../project/diapason-shell-policy-roadmap.md).

## 1. Summary

Adopt Brush for Bash syntax and selected shell semantics, and Regorus for a
single authorization policy language based on Open Policy Agent (OPA) Rego. A
bounded, effect-free analyser produces a graph of possible actions and their
provenance. Rego evaluates that evidence alongside tool identity, workspace
trust, approvals, and runtime capabilities. Rust reduces the findings and
enforces the resulting obligations before execution.

Replace the existing policy authorities through a compatibility migration, not
by adding another permanent veto layer. Every existing policy decision must
remain representable. Incomplete analysis must never silently expand automatic
permission. Operating-system sandboxes, counters, approval storage, and process
launch remain enforcement mechanisms, outside Rego.

Policy drop-ins can add restrictions, contribute authorized grants, or
explicitly supersede an existing policy unit. Supersession requires the
target's exact identity and digest and an authority grant from its owner.
Filename order never grants authority. Rego supplies the policy semantics;
Diapason supplies installation trust, lifecycle, and replacement rules.

## 2. Problem and current state

The repository contains overlapping decision paths with different meanings for
a prefix, an approval, a parse failure, and an unmatched request. The inventory
below is grounded in checkout `7fecc7f7c9dd689e49bb9701900972497642a334`,
inspected on 2026-11-03. Paths retain the current `vtcode` names. This is a
migration inventory, not a claim that every helper is reached by every
executor. It includes Model Context Protocol (MCP) calls and pseudo-terminal
(PTY) execution.

| Mechanism                                                                                                                                                                                      | Current distinction to preserve                                                                                                                             | Target responsibility                                                                           |
| ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------- |
| [CommandPolicyEvaluator](../../crates/codegen/vtcode-core/src/tools/command_policy.rs)                                                                                                         | Raw trimmed text; `starts_with` prefixes, regexes, and globs; deny first; no allow rules means allow; configuration/environment rule merging and text cache | Versioned compatibility facts and Rego policy unit                                              |
| [ShellPolicyChecker](../../crates/codegen/vtcode-core/src/tools/registry/shell_policy.rs)                                                                                                      | Agent deny regex/glob; simplistic `&&`, `\|\|`, `;` splitting; invalid patterns skipped                                                                     | Agent restrictions with explicit legacy splitter identity                                       |
| [Shell parser](../../crates/codegen/vtcode-safety/src/command_safety/shell_parser.rs)                                                                                                          | Tree-sitter command vectors; permissive tokenizer fallback and a separate strict entry point                                                                | Brush syntax plus explicit incomplete analysis                                                  |
| [Unified safety](../../crates/codegen/vtcode-safety/src/command_safety/unified.rs) and [safe command registry](../../crates/codegen/vtcode-safety/src/command_safety/safe_command_registry.rs) | Dormant compatibility path: text cache, danger detection, command-specific rules, and permissive unknown fallback                                           | Preserve relevant fixtures; retire the old evaluator rather than promote it to global authority |
| [Command argument classification](../../crates/codegen/vtcode-core/src/tools/command_args.rs)                                                                                                  | Separate dynamic-syntax and read-only classification                                                                                                        | Common graph facts; no independent allow authority                                              |
| [Execution policy](../../crates/codegen/vtcode-safety/src/exec_policy/policy.rs) and [manager](../../crates/codegen/vtcode-safety/src/exec_policy/manager.rs)                                  | First matching argv-prefix rule; restrictive batch reduction; heuristic prompt fallback; session approvals                                                  | Ordered legacy rule data evaluated by Rego                                                      |
| [ToolPolicyManager](../../crates/codegen/vtcode-core/src/tool_policy.rs)                                                                                                                       | Tool allow/deny/prompt; exact, prefix, scoped-prefix and regex approval entries; derived shorter prefixes                                                   | Tool policy units and explicit approval records                                                 |
| [Policy facade](../../crates/codegen/vtcode-core/src/tools/registry/policy_facade.rs) and [registry policy](../../crates/codegen/vtcode-core/src/tools/registry/policy.rs)                     | Canonical and public aliases, MCP provider policy, safe mode, full-auto allowlist and risk defaults                                                         | Normalized identity and contextual policy inputs                                                |
| [Approval learning](../../crates/codegen/vtcode-core/src/tools/registry/justification.rs) and [recorder](../../crates/codegen/vtcode-core/src/tools/registry/approval_recorder.rs)             | Persisted counts and approval-rate threshold                                                                                                                | Evidence and suggestions; no self-issued authority                                              |
| [SafetyGateway](../../crates/codegen/vtcode-core/src/tools/safety_gateway.rs)                                                                                                                  | Rate limits, dotfile protection, command policy, trust/preapproval and risk ordering                                                                        | Contextual findings plus stateful enforcement                                                   |
| [Execution adapter](../../crates/codegen/vtcode-core/src/tools/registry/executors.rs) and [MCP sandbox](../../crates/codegen/vtcode-safety/src/mcp_sandbox.rs)                                 | Final execution preparation and platform sandbox enforcement                                                                                                | Obligations enforced at dispatch, with no unsandboxed fallback                                  |
| [Documented sandbox and native-plugin boundaries](../development/COMMAND_SECURITY_MODEL.md)                                                                                                    | Additional-permission normalization, escalation conflicts, path checks, and consent before native library initialization                                    | Preserve pre-effect checks as typed policy obligations and runtime enforcement                  |

_Table 1: Existing responsibilities and their intended destination._

The old unified evaluator's production-source caller is the deprecated
`CommandTool` preparation path, marked dead outside tests. In contrast,
`command_args.rs` guards also feed the active tool-intent read-only classifier.
The separate
[dangerous-command detector](../../crates/codegen/vtcode-safety/src/command_safety/dangerous_commands.rs)
can ignore a nested-shell parse failure, while strict read-only classification
rejects it. Capture both behaviours before changing them.

For example, a raw-text prefix `git` and an argv prefix `["git"]` are not
equivalent. Nor is the first matching execution rule equivalent to collecting
all matching restrictions. The importer must preserve these distinctions,
including negative cases. It must not translate every old rule into a generic
`regex.match` or treat a subsystem's permissive default as a global grant.

The existing
[shell snapshot](../../crates/codegen/vtcode-core/src/tools/shell_snapshot.rs)
records environment variables, shell identity, age, and configuration-file
fingerprints. It does not represent aliases, function definitions, options,
traps, or all other state that changes command meaning. A cached approval for
text alone cannot establish that the same operation will run again.

## 3. Goals and non-goals

### 3.1. Goals

1. Give Bash, direct argv, native tools, and Model Context Protocol (MCP)
   operations one versioned authorization contract and one final authority.
2. Represent all existing rule families, including legacy matching, tool
   permissions, risk thresholds, trust, approval learning, capability requests,
   sandbox constraints, and execution budgets.
3. Describe possible shell effects, uncertainty, integrity provenance, and
   confidentiality provenance without executing the proposed program.
4. Support local drop-ins, reusable Rego modules and data, additive extension,
   parameter changes, and authorized replacement with inspectable provenance.
5. Explain decisions using rule identities, graph nodes, source spans,
   uncertainty reasons, and unmet enforcement obligations.
6. Remove redundant parsers and policy evaluators after evidence-backed
   cutover; keep a time-bounded compatibility reader for old configuration.

### 3.2. Non-goals

The first release does not replace the execution shell with Brush, promise a
complete prediction of arbitrary Bash, or inspect arbitrary executable
internals. It does not replace kernel confinement, introduce remote policy
execution, automatically rewrite commands, or let a model approve its own
requests. It does not rename existing crates or configuration directories.

Other shell dialects remain explicitly identified and opaque unless they have a
separately validated analyser. A successful Bash parse is not evidence of
PowerShell, zsh, or fish semantics.

## 4. Proposed design

### 4.1. Evidence before authorization

The runtime normalizes a request and captures its execution state. A
Brush-backed analyser emits a versioned effect graph. Nodes describe external
calls, builtins, redirections, environment changes, tool actions, and opaque
effects. Edges describe control flow, calls, data flow, and dependencies.
Reachability and effect completeness are separate dimensions: a definitely
reached call can still have unknown downstream effects.

Values carry independent integrity and confidentiality labels. Quoting changes
shell expansion behaviour; it does not make a secret public or an untrusted
pathname safe. Unknown external output and status remain unknown; the analyser
explores both feasible branches or records an opaque remainder.

Brush's current `ShellExtensions` includes `ProcessLauncher::spawn` and `exec`,
correcting the earlier discussion's statement that no process hook exists.[^1]
However, `LaunchRequest` already contains open file descriptors, and builtins
and functions do not reach that hook.[^2] Blocking process launch alone cannot
prevent redirection effects or provide symbolic branching. Use `brush-parser`
directly and reuse audited pure semantics selectively. Treat deeper
`brush-core` integration as a feasibility gate, not a supplied abstract
interpreter.

### 4.2. Policy findings and enforcement

Each active policy unit returns a defined result envelope containing typed
findings: `deny`, `review`, `grant`, `constrain`, and `advise`. The host
attaches authenticated unit and generation provenance; modules cannot assert
their own authority. Grants cover specified actions and require delegations.
Constraints intersect. Denials dominate reviews and grants. Missing coverage
requires review; invalid evaluation blocks execution.

An approval discharges named review obligations for a bound request. It does
not erase a denial, satisfy an unavailable sandbox capability, or override a
policy-engine failure. Rego sees immutable budget and approval facts; Rust
atomically reserves budget and validates approvals immediately before use.

### 4.3. Drop-ins, extension, and supersession

The host loads policy units from built-in, administrator, user, trusted
workspace, and session sources. Source trust is assigned by installation and
delegation, never by a field inside an untrusted file. Untrusted workspaces do
not activate their own modules. Within a trusted source, multiple Rego files
can contribute to the same unit's set-valued findings. Separate units use
separate namespaces and evaluation compartments.

Adding a restriction requires no power to grant execution. Adding a grant
requires an explicit scope delegated by the installation authority. Changing
parameters or replacing a unit is an authority-bearing update, not an innocent
data merge. A replacement names the old unit and exact digest, and requires the
owner's replacement permission. The loader resolves the active set before
evaluation, rejects ambiguous replacement graphs, and publishes a complete
immutable generation atomically.

Ship immutable enforcement invariants and mandatory administrator controls
separately from replaceable defaults. For example, a user may replace the
default Git-publish review policy when delegated that ability; the same unit
cannot replace an administrator's secret-egress prohibition. A workspace can
extend the effective policy with a production-branch review requirement but
cannot relax its user's restrictions.

OPA's declarative rules and bundle roots are useful building blocks, not a
built-in inheritance or authority system.[^3] Diapason must specify the loader
contract explicitly. It must not use file ordering, duplicate complete rules,
or Rego's test-oriented `with` overrides as a production supersession scheme.

## 5. Requirements and acceptance criteria

| ID  | Requirement                                                    | Acceptance evidence                                                                                                               |
| --- | -------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------- |
| R1  | Every existing decision family is representable                | Inventory maps each reachable decision and bypass to a compatibility fixture, Rego unit, enforcer, and retirement task            |
| R2  | Unknown evidence cannot silently increase automatic permission | Abstract-domain and budget-exhaustion properties; no empty-success parse fallback; adversarial branch fixtures                    |
| R3  | Lower authority cannot weaken higher controls                  | Exhaustive small authority-graph checks, namespace-injection cases, and replacement conflict tests                                |
| R4  | All routes use the common boundary                             | End-to-end cases for shell/argv, PTY, native tools, MCP calls/server launch, delegated agents, safe mode, and full-auto           |
| R5  | The operation executed is the operation authorized             | Request, state, policy, approval, and sandbox binding checks; mutation and replay cases                                           |
| R6  | Policy errors never become permission                          | Load/evaluation/schema/timeout faults block; required results are always defined                                                  |
| R7  | Policy authors can extend and replace safely                   | Check, test, effective-policy and explain commands show origin, delegation, supersession and unmet obligations                    |
| R8  | Old mechanisms are actually retired                            | Reachability audit shows no old allow decision in authoritative mode; legacy helpers survive only as pure matchers where required |
| R9  | Sensitive values stay out of routine diagnostics               | Secret canaries absent from logs, events, prompts, caches on disk, and policy test reports                                        |

_Table 2: Acceptance requirements for the replacement, not claims of delivery._

The
[design](../design/diapason-shell-policy-design.md#10-verification-and-release-evidence)
defines the limits of these proofs. Differential agreement cannot prove Bash
soundness, and passing an observed corpus cannot establish that a command model
covers arbitrary plugin or subprocess behaviour.

## 6. Compatibility and migration

First freeze the current effective decision paths in fixtures, including their
order, matching dialect, malformed input, environment-derived rules, cache
scope, and approval behaviour. Import configuration as data tagged by its
source and legacy semantics. Rego expresses the decision logic; narrow Rust
matchers can preserve exact legacy regex/glob behaviour without remaining
independent authorization engines.

Run the new evaluator in shadow mode while the old system remains
authoritative. Compare effective outcomes and obligations, not just booleans.
Do not execute a second copy of commands to obtain comparisons. Classify each
difference as an importer defect, analyser uncertainty, old-system defect, or
intentional hardening. Neither agreement nor a high approval rate authorizes a
broader grant.

Next enable an opt-in guarded mode: new execution requires both old permission
and satisfaction of the new result. Only a reviewed migration changes old
fail-open defaults, invalid-pattern handling, or broad learned approvals.
Representability does not require preserving unsafe behaviour as the new
default. Old approvals remain inspectable, but missing state bindings require
renewed approval rather than an invented scope.

Cut over one audited route at a time to the common enforcer. Release-wide
completion requires every route in R4 and removal of competing authorities.
Rollback selects a previously validated compatible generation; it cannot
restore approvals invalidated by newer mandatory restrictions or silently
resurrect deprecated fail-open paths. See design §§8–10 for activation,
cutover, and rollback details.

## 7. Prior art and alternatives

| Source or alternative                                                   | Adopt                                                                                           | Deliberate difference                                                                                        |
| ----------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------ |
| [Codex rules](https://learn.chatgpt.com/docs/agent-configuration/rules) | Layered drop-ins, strictest matching result, author-supplied examples, command-line explanation | Semantic shell evidence and authority-checked replacement; argv prefixes alone do not cover arbitrary Bash   |
| [CC Safety Net](https://github.com/kenryu42/cc-safety-net)              | Destructive-command and secret-access cases, wrapper awareness, reusable rulebooks              | Rego composition and graph provenance; its README explicitly excludes sandbox containment                    |
| [Cupcake](https://github.com/eqtylab/cupcake)                           | Interception, evidence enrichment, policy outside model context, structured feedback            | Embedded Regorus rather than its documented OPA-to-WebAssembly path; no automatic command modification in v1 |
| Keep Tree-sitter and add more regexes                                   | Lowest migration cost                                                                           | Retains multiple authorities and loses expansion/control-flow provenance                                     |
| Run Brush with external execution disabled                              | Concrete resolved calls can be observed                                                         | Builtins, expansion, redirections and unknown exits prevent this being safe abstract execution               |
| Replace GNU Bash execution with Brush immediately                       | Potentially fewer semantic discrepancies                                                        | Introduces shell compatibility and job-control changes before policy consolidation is proven                 |
| OPA sidecar or compiled WebAssembly                                     | Mature separate deployment model                                                                | Adds deployment/compiler/runtime boundaries; Regorus fits the requested Rust embedding                       |
| Bashkit or a different analyser                                         | Worth a bounded fallback experiment if feasibility fails                                        | Would reopen the explicitly chosen Brush foundation; not an implicit scope change                            |

_Table 3: Prior-art lessons and rejected first-release alternatives._

Regorus documents Rego v1 and an OPA conformance baseline with builtin gaps,
including `globsmatch`.[^4] The researched upstream manifests reported
`brush-parser` 0.4.0 and Regorus 0.14.1 on their main branches.[^5] These are
research observations, not selected dependency releases. Pin audited versions,
features, licences, and minimum Rust compatibility in the feasibility slice. Do
not infer support for every current OPA builtin from Rego v1 support.

## 8. Open questions

- Which pinned Brush parser backend and Regorus evaluation backend meet the
  workspace compiler baseline and bounded-execution requirements? Record
  measured evidence before selecting dependencies.
- Which default policy units should users be allowed to supersede? The
  proposed default is no replacement permission unless explicitly declared.
- Which platform sandbox profiles can contain opaque shell effects while
  offering useful review-based execution? Unsupported profiles must block.
- What analysis and evaluation budgets meet the interactive latency target
  on supported hardware? Initial limits in the design are experiment bounds,
  not published performance guarantees.

These questions gate implementation acceptance. They do not defer the
non-negotiable authority, uncertainty, and enforcement rules in this RFC.

## 9. Recommendation

Adopt the Brush-plus-Regorus architecture and validate it through the linked
roadmap. Accept the policy and evidence contracts before replacing execution
paths. Keep this RFC Proposed until review settles the remaining dependency,
delegation, and containment decisions. Approval of this draft's outline is not
approval to implement or weaken an existing policy.

## References

[^1]: [Brush shell extension source](https://github.com/reubeno/brush/blob/main/brush-core/src/extensions.rs),
    inspected through Firecrawl on 2026-11-03.
[^2]: [Brush process request source](https://github.com/reubeno/brush/blob/main/brush-core/src/processes.rs).
[^3]: [OPA policy language](https://www.openpolicyagent.org/docs/policy-language)
    and [bundle model](https://www.openpolicyagent.org/docs/management-bundles).
[^4]: [Regorus conformance documentation](https://github.com/microsoft/regorus#opa-conformance).
[^5]: [Brush parser manifest](https://github.com/reubeno/brush/blob/main/brush-parser/Cargo.toml)
    and [Regorus manifest](https://github.com/microsoft/regorus/blob/main/Cargo.toml).
