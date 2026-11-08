# Diapason Developer Guide

- [Development setup and workflows](./development/README.md)
- [Architecture](./ARCHITECTURE.md)
- [ACP runtime](./development/acp-runtime.md)
- [Custom-provider request policy](./development/CUSTOM_PROVIDER_REQUEST_POLICY.md)
- [Apply-patch safety contract](./development/patch-safety-contract.md)
- [Diapason shell-policy proposal](./rfcs/rfc-0001-diapason-shell-policy.md):
  a proposal that changes no runtime APIs or dependencies. The
  [technical design](./design/diapason-shell-policy-design.md) is the source
  for the architecture; the
  [roadmap](./project/diapason-shell-policy-roadmap.md) tracks feasibility and
  delivery.
- [Testing](./development/testing.md)
- [Instruction discovery boundary](./development/instruction-discovery.md)
- [Configuration field reference](./config/CONFIG_FIELD_REFERENCE.md)
- [Documentation style guide](./documentation-style-guide.md)
- [0.157.0 release notes](./releases/0.157.0.md) - Breaking API renames and
  migration actions, including the `catalog` to `catalogue` Rust API rename and
  the external spellings that remain unchanged.

## Catalogue naming

Rust APIs use the native `catalogue` spelling: `ToolCatalogue`,
`ToolCatalogueEntry`, `ToolCatalogueSource`, `CatalogueToolKind`,
`SessionToolCatalogue`, `SessionToolCatalogueSnapshot`,
`SessionToolCatalogueState`, `PodCatalogue`, `ModelCatalogueEntry`,
`MergeCatalogue*`, `MergeGatewayCatalogueClient`, `PodsStore::catalogue_path`,
`PodsStore::load_catalogue`, `PodsStore::save_catalogue`, `catalogue_tools`,
`model_catalogue_entry`, `catalogue_provider_keys`, `ResolvedModel::catalogue`,
and the `tool_catalogue_facade` and `pods::catalogue` modules. The old
spellings are removed rather than deprecated, so in-repository callers must use
the new names; the full removed/replacement list is in the
[0.157.0 release notes](./releases/0.157.0.md#catalogue-spelling-renames).

Compatibility applies only to spellings that leave the Rust API:

- Configuration keys, wire protocol fields and telemetry metric names keep
  their canonical `catalog` spelling. Rename the Rust item, not the serialized
  name, and keep an explicit `#[serde(rename = "...")]` where a field moves.
- The pod store keeps reading and writing `catalog.json`. Only the accessor
  name changes, so existing deployment profiles continue to load.
- Runtime prompt metadata uses the `[Runtime Tool Catalogue]` header.
  Refreshing a prompt removes a legacy `[Runtime Tool Catalog]` section while
  preserving notices appended after it, and `stable_system_prefix_hash` ignores
  metadata under either header.

Do not add aliases for the old Rust names, and do not rewrite persisted data to
match the new spelling.
