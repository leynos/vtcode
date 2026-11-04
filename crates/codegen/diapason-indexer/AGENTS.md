# diapason-indexer guidance

[Root guidance](../../../AGENTS.md) applies. See [crate README](README.md).

- Keep indexing bounded, deterministic and respectful of ignored/generated
  paths.
- Reuse shared filesystem containment and hashing helpers; do not read outside
  authorized roots.
- Preserve cancellation and error reporting for large or inaccessible
  workspaces. Detailed module guidance:
  [diapason-indexer](../../../docs/development/module-guidance/diapason-indexer.md).
