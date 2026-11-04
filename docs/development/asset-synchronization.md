# **Asset Synchronization**

This guide explains how embedded assets are consumed by the `diapason-core`
crate.

## **Overview**

Diapason embeds selected workspace assets directly into the `diapason-core`
crate at build time. The `build.rs` script resolves these assets from the
workspace root, so there is no separate mirror directory to maintain.

## **Synchronized Assets**

The following asset is currently embedded:

| Source (Workspace)                  | Embedded Destination                                | Purpose                                  |
| ----------------------------------- | --------------------------------------------------- | ---------------------------------------- |
| `docs/modules/diapason_docs_map.md` | `docs/diapason_docs_map.md` (in crate build output) | Documentation map for self-documentation |

## **How It Works**

1. **Single Source of Truth**: Assets are maintained at the workspace root
   (e.g., `docs/modules/diapason_docs_map.md`).
2. **Build-Time Embedding**: `crates/codegen/diapason-core/build.rs` copies the
   asset into the crate's build output during compilation.
3. **No Manual Sync Required**: Because the build script reads directly from
   the workspace root, there is no separate sync step or mirror directory.

## **Verification**

After modifying an embedded asset:

```bash
# Verify the crate still builds
cargo check --package diapason-core

# Run tests
cargo nextest run -p diapason-core
```

## **Adding New Embedded Assets**

To embed a new asset:

1. **Add to `build.rs`**: Append a `(source_relative, dest_relative)` tuple to
   `EMBEDDED_ASSETS` in `crates/codegen/diapason-core/build.rs`.
2. **Test the Build**: Ensure `cargo check --package diapason-core` succeeds and
   the asset is available at runtime.

## **Related Documentation**

- **[Development Guide](./README.md)** - Development overview

______________________________________________________________________

## **Navigation**

- **[Back to Development Guide](./README.md)**
