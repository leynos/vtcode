# Tool catalogue naming and compatibility

- Status: accepted
- Applies to: 0.157.0 and later
- Related:
  [0.157.0 release notes](../releases/0.157.0.md#catalogue-spelling-renames)
- Related: [developer guide](../developers-guide.md#catalogue-naming)

## Context

Diapason's Rust API mixed two spellings of the same concept. Types such as
`ToolCatalog` and `SessionToolCatalog` used the American `catalog`, while the
project's documentation, prompts and user-facing text used the British
`catalogue`. The split made symbol search unreliable, produced near-duplicate
names in review diffs, and left contributors guessing which spelling a new item
should take.

The spelling is not purely internal. Some `catalog` strings are part of
contracts that outlive a release:

- configuration keys read from `config.toml`;
- wire fields exchanged with providers and ACP clients;
- telemetry metric names consumed by dashboards;
- the persisted pod file `catalog.json`;
- runtime prompt section headers that embedders may already supply.

Renaming those strings would break existing deployments, dashboards and
third-party prompts. Renaming only the Rust items keeps the source tree
consistent without changing any external contract.

## Decision

1. Rust items use the native `catalogue` spelling. This covers types, traits,
   functions, modules and fields, for example `ToolCatalogue`,
   `SessionToolCatalogueSnapshot`, `PodsStore::load_catalogue`,
   `catalogue_provider_keys` and the `pods::catalogue` module.
2. Old Rust spellings are removed, not deprecated. No `#[deprecated]` aliases
   and no `pub use` shims are added, so the compiler reports every
   in-repository caller that still uses the old name.
3. Serialized names keep their existing `catalog` spelling. Where a Rust field
   is renamed, the serialized name is pinned with an explicit
   `#[serde(rename = "...")]` rather than relying on a default.
4. Persisted data is never rewritten. The pod store continues to read and write
   `catalog.json`; only the accessor names change.
5. Runtime prompt metadata uses the `[Runtime Tool Catalogue]` header.
   Refreshing a prompt removes a legacy `[Runtime Tool Catalog]` section, keeps
   notices that follow it, and `stable_system_prefix_hash` ignores metadata
   under either header so prompt-cache prefixes stay stable across the rename.

## Consequences

- A single spelling is searchable across the workspace, and new items have an
  unambiguous name.
- Library consumers must update call sites; the
  [release notes](../releases/0.157.0.md#catalogue-spelling-renames) list every
  removed and replacement name.
- External contracts are unchanged, so configuration files, dashboards, ACP
  clients and existing pod stores keep working without migration.
- The rename is mechanical and reviewable: a diff that changes a serialized
  name, a metric name or a persisted filename is a defect, not a rename.

## Alternatives considered

- **Keep both spellings and add aliases.** Rejected: aliases hide stale call
  sites and double the public surface for no behavioural gain.
- **Rename the serialized names too.** Rejected: it breaks deployed
  configuration, telemetry dashboards and existing pod stores for a cosmetic
  change.
- **Rewrite persisted data to the new spelling.** Rejected: it would require a
  migration path and would strand older binaries reading the same workspace.
