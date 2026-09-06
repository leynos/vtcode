# Prompt Architecture

## Cache-stable segments

Each harness segment freezes one `SessionRequestEnvelope`: a segment ID, the
system prompt, a canonically ordered tool catalogue, and the instruction digest.
Ordinary turns only append messages. Compaction or a change to the catalogue
epoch, primary agent, model, provider, mode, or instruction digest starts a new
segment and therefore one intentional cache miss.

Session-start time is rendered once into the cached system prompt. Later time
must come from tool output. Approval and sandbox policy values are execution
gateway state and must not be rendered into prompts or tool descriptions.

VT Code's prompt system follows the four pillars of *Section 18.3 (Prompt
Architecture)* of the agentic-AI guide:

1. **System-prompt design** (18.3.1) — persona, capabilities, constraints,
   universal runtime guidance, and output format assembled once per segment.
2. **Dynamic assembly** (18.3.2) — modular, cache-friendly composition at
   segment creation from the base prompt, instruction appendix, runtime
   contract, recovery mode, harness limits, tool catalogue, and primary-agent
   state. Later runtime changes are appended as context messages.
3. **Few-shot management** (18.3.3) — keyword-tagged examples loaded from
   disk, token-budgeted via `vtcode_commons::tokens::estimate_tokens`,
   and appended as a synthetic system context message when relevant; they do
   not rewrite the immutable prompt prefix.
4. **Tool descriptions** (18.3.4) — every LLM-visible tool must include
   when-to-use guidance, when-NOT-to-use guidance, and a constraints cue
   (rate-limit, max size, side-effect, permission). The
   `tool_descriptions_satisfy_documented_contract` test in
   `crates/codegen/vtcode-core/src/tools/registry/builtins.rs::tests` enforces this
   contract at `cargo test` time.

## Assembly order

At segment creation, `request_builder::build_prompt_output` produces the
immutable `system_prompt` in this order:

1. **Base system prompt** — `vtcode_core::prompts::system::generate_system_instruction`
   (with `Default / Minimal / Lightweight / Specialized` variants cached
   via `OnceLock`). Each cached profile includes the deterministic compiled
   runtime-guidance section from `prompts/runtime_guidance.rs` exactly once.
2. **INSTRUCTIONS** appendix — `AGENTS.md`, `CLAUDE.md`, and other
   project-scoped instruction files discovered by
   `project_doc::build_instruction_appendix_with_context`.
3. **Runtime mode contract** — full-auto / planning / request_user_input
   flags (`runtime_contract::append_runtime_mode_sections`).
4. **Auto-permission notice** — only when `auto_permission && !planning`.
5. **Active Primary Agent Skills** — when a primary agent is active.
6. **Harness Limits** — `harness_limits::upsert_harness_limits_section`.
7. **Recovery Mode** — when `tool_free_recovery` is active; tools are
   stripped from the catalogue snapshot.
8. **Runtime Tool Catalogue** — `append_runtime_tool_prompt_sections` with
   the planning/capability filtered `SessionToolCatalogueSnapshot`.
9. **GitHub Copilot Client Tools** — only for the Copilot provider.
10. **Active Primary Agent Runtime State** — model, reasoning effort,
    instructions, and `### Memory Appendix` if the agent has memory.
11. **[Few-Shot Examples]** — appended after the immutable prompt as a
    synthetic system context message when relevant and budget allows (see
    below).

## Few-shot management (Section 18.3.3)

### Authoring examples

Examples live as Markdown files under:

- `<workspace>/.vtcode/prompts/examples/*.md`
- `<canonical user config directory>/prompts/examples/*.md` (legacy user prompt files are migrated there)

The filename stem is the example id. The file body uses YAML frontmatter
for metadata:

```markdown
---
id: read-then-edit-large-file
tags: [read, edit, patch, large-file, refactor]
summary: Inspect a large file in targeted shell slices before editing; use apply_patch for multi-hunk changes.
---
# User
<the user query>

# Assistant
<the expected tool sequence and rationale>
```

`tags` and `summary` are optional. `tags` drive the keyword selector;
`summary` is appended to the prompt as a one-line caption above the
body. Workspace examples take precedence over user-global examples on id
collision.

### Selection

For each turn, the harness:

1. Reads the most recent user message and uses it as the selection
   query.
2. Loads the example store from disk via `FewShotStore::load`.
3. Scores each example: +1.0 per exact tag/word match, +0.5 per tag that
   appears as a substring of the query.
4. Sorts by score desc, then id asc (stable ordering).
5. Walks in order, appending until the running total exceeds
   [`DEFAULT_FEW_SHOT_BUDGET_TOKENS`] (default 800 tokens, ~10% of an 8K
   context window).
6. Renders the chosen examples as a `[Few-Shot Examples]` block appended
   to the system prompt before the tool catalogue.

The selection is keyword-based and runs in-process without an embedding
provider. Embedding-based selection is the documented next step (see
Section 18.3.3); layering it on top will not require API changes.

### Guard rails

- **Recovery mode skips few-shot.** When `tool_free_recovery` is active
  the model is in "summarize from evidence" mode and adding examples
  would distract. The few-shot block is omitted.
- **Empty stores are silent.** If no examples are present, no
  `[Few-Shot Examples]` block is added — no overhead for users who don't
  ship examples.
- **Token count is honest.** `FewShotExample::token_count` is computed
  via the `cl100k_base` BPE tokenizer in
  `vtcode_commons::tokens::estimate_tokens`. Budget enforcement uses the
  same tokenizer, so the budget is accurate for OpenAI models and
  within ~10% for Anthropic / Gemini.

### Adding a new example

1. Choose a stable, hyphenated id (used in logs and prompts).
2. Author the `.md` file with frontmatter and body.
3. Run `cargo test -p vtcode-core prompts::few_shot::tests` to confirm
   the example loads and selects correctly.
4. Optional: add a `FewShotStore::from_examples(...)` unit test that
   asserts the example is selected by a representative query.

## Tool description contract (Section 18.3.4)

Every LLM-visible tool whose description is not in the allowlist must
include:

- A **verb cue** — `Use `, `Create `, `List `, `Fetch `, etc. — so the
  model recognizes the action the tool performs.
- An **anti-pattern cue** OR a **constraint cue** — e.g. `Do NOT ...`,
  `Avoid ...`, `sparely`, or `max ...`, `rate-limit`, `session`,
  `Prompt`, `timeout`, `inherits` — so the model knows the limits and
  side effects.

Tools exempted from the anti-pattern/constraint requirement are
single-action or read-only helpers (`cron` action=`list`/`delete`,
`mcp` action=`list_servers`, etc.) where the model can safely call them without
explicit guard-rails. See the test for the full allowlist and cue
vocabularies.

Run `cargo test -p vtcode-core tools::registry::builtins::tests::tool_descriptions_satisfy_documented_contract`
to validate any description change before merging.

[`DEFAULT_FEW_SHOT_BUDGET_TOKENS`]: ../crates/codegen/vtcode-core/src/prompts/few_shot.rs
