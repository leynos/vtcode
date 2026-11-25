# Vendored `openai-harmony` patch

This directory is the crates.io release of `openai-harmony 0.0.8` with one
unused dependency declaration removed. It is applied to the workspace through
`[patch.crates-io]` in the repository root `Cargo.toml`.

## Source

- Crate: `openai-harmony`
- Version: `0.0.8` (unchanged)
- Archive:
  `https://static.crates.io/crates/openai-harmony/openai-harmony-0.0.8.crate`
- Archive SHA-256:
  `e77e82af451fc95deeb728a40b84db8ee82d341e136c268de415123a560b9b72`
- Upstream source revision recorded in `.cargo_vcs_info.json`:
  `ec7606df9e87e3d0a1fec9f50928c1e407f0c438`
- Licence: Apache-2.0 (`LICENSE`, untouched)

The archive checksum identifies the **upstream archive**, and it matches the
`openai-harmony` entry that was in `Cargo.lock` before the patch was
introduced. It does not checksum this working tree, which is edited. The tree
is verified by extracting that archive into a scratch directory and comparing
it with `patches/openai-harmony`; the only differences must be the ones listed
under "Local delta".

## Why

The RustSec advisory database entry `RUSTSEC-2024-0436` reports `paste 1.0.15`
as unmaintained. Diapason reached `paste` only through the default formats of
`image`:

```text
image -> ravif -> rav1e -> paste
image -> exr   -> pulp  -> paste
```

Those formats were not declared by Diapason. `openai-harmony 0.0.8` declares
`image = "0.25.6"` with default features, and never uses it:

```sh
grep -rni image src/
# no matches
```

Cargo features are additive, so the workspace's own `default-features = false`
declaration on `image` could not subtract the defaults `openai-harmony`
requests. Removing the dead declaration is the narrowest fix.

## Local delta

Only the unused dependency was removed from the build, and only the crate
payload was retained. Relative to the published archive:

- `Cargo.toml` and `Cargo.toml.orig`: the `[dependencies.image]` block
  (`version = "0.25.6"`) and the `image = "0.25.6"` line were deleted, and each
  file gained a "MODIFIED by Diapason" header comment.
- `Cargo.lock` was deleted rather than pruned. A path dependency's lockfile has
  no effect on resolution, and deleting it leaves no stale `image`-only
  resolution to review.
- Non-Rust upstream trees were not vendored: `demo/`, `javascript/`, `python/`,
  `tests/test_harmony.py`, `.github/`, `.gitignore`, `AGENTS.md`,
  `pyproject.toml`, `run_checks.sh` and `test_python.sh`. None is referenced by
  the crate's Rust build, and the Python test cannot run without the excluded
  Python wrapper.
- `README.md` and `docs/format.md`, `docs/python.md`, `docs/rust.md` were
  reflowed by the repository Markdown formatter (`mdtablefix`) so the workspace
  `check-fmt` gate accepts the vendored tree, and each gained a "MODIFIED by
  Diapason" notice. Prose, code fences and links are otherwise unchanged; only
  line wrapping differs. Apache-2.0 section 4(b) requires modified files to
  carry a prominent change notice, which is why those documents and the two
  manifests carry one.

`version = "0.0.8"`, `LICENSE`, `src/`, `test-data/` and `docs/header.png` are
byte-identical to the published archive. Every other difference is listed above.
`README.md` is compiled into rustdoc through `include_str!` in `src/lib.rs`;
reflowing it changes no rendered content.

## Upstream

- Pull request:
  [openai/harmony#115](https://github.com/openai/harmony/pull/115), "Remove
  unused `image` dependency (all `core2` versions yanked)".
- Commit: `fc7e8837a348c8248087cef6b956695b8f0782c5`.
- Status when this patch was created: open and unmerged; `openai-harmony` was
  still `0.0.8` on crates.io, so no released version carries the fix.

## Exit criterion

Delete this directory and the matching `[patch.crates-io]` entry when a released
`openai-harmony` drops `image`, disables its default features, or otherwise
stops resolving `paste`. Then:

1. bump the `openai-harmony` requirement in
   `crates/codegen/diapason-llm/Cargo.toml` to the fixed release,
2. delete `patches/openai-harmony`,
3. remove `openai-harmony = { path = "patches/openai-harmony" }` from
   `[patch.crates-io]` and `patches/openai-harmony` from `workspace.exclude` in
   the root `Cargo.toml`,
4. regenerate `Cargo.lock`,
5. confirm `cargo tree --locked -i paste` stays empty and
   `cargo audit --file Cargo.lock` reports no `RUSTSEC-2024-0436`.

## Scope

`[patch.crates-io]` is local to this workspace. It does not reach consumers of
a published `diapason-llm`. See
[`docs/execplans/rustsec-2024-0436-paste-image-backends.md`](../../docs/execplans/rustsec-2024-0436-paste-image-backends.md)
for the full record.
