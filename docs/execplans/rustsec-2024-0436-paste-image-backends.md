# Remove the unmaintained `paste` path behind `image` codecs

Status: implemented under issue
[#119](https://github.com/leynos/vtcode/issues/119). This record covers the
RustSec remediation only; the `cargo-unmaintained` scanner baseline is owned
separately by [#108](https://github.com/leynos/vtcode/issues/108) and
[`issue-108-unmaintained-baseline.md`](issue-108-unmaintained-baseline.md).

## Purpose and constraints

`cargo audit --file Cargo.lock` reported `paste 1.0.15` as unmaintained under
`RUSTSEC-2024-0436`. Removing the advisory without suppressing it, without an
allow-list entry, and without an unrelated dependency upgrade required finding
the activator rather than the crate.

The direct workspace declaration is already minimal:

```toml
image = { version = "0.25.10", default-features = false, features = ["jpeg", "png", "gif", "webp"] }
```

`diapason-ui` consumes it for clipboard image decoding. The resolved advisory
paths were:

```text
image 0.25.10 -> ravif  -> rav1e 0.8.1 -> paste 1.0.15
image 0.25.10 -> exr    -> pulp  0.22.3 -> paste 1.0.15
```

Neither path comes from a codec the workspace declares. The resolver reached
them through `image`'s default feature set, which carries `default-formats`,
which in turn enables every bundled format including `avif` (through `ravif` and
`rav1e`) and `exr` (through `exr` and `pulp`).

## Evidence chain

`cargo tree --locked -e features -i image` before the change showed:

```text
image feature "avif"
└── image feature "default-formats"
    └── image feature "default"
        └── openai-harmony v0.0.8
            └── openai-harmony feature "default"
                └── diapason-llm v0.156.1
```

The activator is `openai-harmony 0.0.8`, reached through `diapason-llm`. Its
manifest declares:

```toml
[dependencies.image]
version = "0.25.6"
```

No file under `src/`, `tests/` or `demo/` in that crate references `image`. The
dependency is declared and never used.

## Why the direct declaration cannot suppress it

Cargo features are additive. A dependency's feature set is the union of every
request that reaches it, regardless of which package asked. The workspace
declaration in the root `Cargo.toml` uses `default-features = false`, which
removes only the defaults *this* declaration would otherwise request. It cannot
subtract the defaults requested by `openai-harmony`, which uses the plain
`[dependencies.image]` form and therefore enables `image`'s `default` feature.

`cargo tree` confirms the effect: `avif` and `exr` were active even though
`diapason-ui` never selected them. A `--no-default-features` build of the
workspace is not an option because the activator is a transitive registry
crate, not a workspace member.

## Selected remediation

Vendor `openai-harmony 0.0.8` under `patches/openai-harmony` and remove only
the unused `image` declaration, then apply it through the existing
`[patch.crates-io]` mechanism that already carries `simdutf`.

- Source archive:
  `https://static.crates.io/crates/openai-harmony/openai-harmony-0.0.8.crate`.
- Archive SHA-256, verified against the `openai-harmony` checksum in the
  pre-change `Cargo.lock`:
  `e77e82af451fc95deeb728a40b84db8ee82d341e136c268de415123a560b9b72`.
- Local delta: the `[dependencies.image]` block removed from `Cargo.toml`, the
  `image = "0.25.6"` line removed from `Cargo.toml.orig`, and the vendored
  `Cargo.lock` deleted rather than pruned. The lockfile has no effect on a path
  dependency; deleting it keeps the vendored tree free of a stale `image`-only
  resolution instead of hand-editing 311 packages.
- Only the crate payload was vendored. Upstream's `demo/`, `docs/`,
  `javascript/`, `python/`, Python test, `.github/`, `AGENTS.md`,
  `pyproject.toml` and shell scripts were not copied; none is referenced by the
  Rust build. `README.md` was reflowed by `mdtablefix` so the workspace
  `check-fmt` gate accepts the vendored tree; prose, fences and links are
  unchanged.
- Everything the build uses is unchanged: `version = "0.0.8"`, `LICENSE`,
  `src/` and `test-data/`.
- Upstream fix: [openai/harmony#115](https://github.com/openai/harmony/pull/115)
  (commit `fc7e8837a348c8248087cef6b956695b8f0782c5`) makes the same removal.
  It was still open and unmerged when this record was written, and
  `openai-harmony` remained at `0.0.8` on crates.io, so no released version
  carries the fix.
- Provenance detail lives in `patches/openai-harmony/VTCODE-PATCH.md`.

The lockfile change is a pure prune. `Cargo.lock` loses the `image` default
format stack and gains nothing: `aligned`, `aligned-vec`, `arbitrary`,
`arg_enum_proc_macro`, `av-scenechange`, `av1-grain`, `avif-serialize`,
`bit_field`, `bitstream-io`, `equator`, `exr`, `imgref`, `lewb`-family codec
helpers, `paste`, `pastey 0.1.1`, `pulp`, `qoi`, `rav1e`, `ravif`, `rgb`,
`v_frame`, `y4m` and the supporting helpers are removed. No surviving package
changes version. The single non-deletion line is an artefact of the prune: with
`pastey 0.1.1` gone, `rig-core`'s dependency on `pastey` no longer needs a
version disambiguator.

## Rejected options

- **Drop or narrow the direct `image` declaration.** It is already
  `default-features = false` with exactly the four declared codecs. Narrowing
  it further would remove formats the UI supports, and changing it cannot
  affect features activated by another package.
- **Remove `arboard`'s `image-data` default.** `arboard` already uses
  `default-features = false` on `image` with platform-appropriate codecs (`png`
  on unix, `png`/`bmp` on Windows, `tiff` on macOS). It contributes only
  `image feature "png"` and is not on either advisory path. Disabling
  `image-data` would remove clipboard image support entirely.
- **Upgrade a dependency to a maintained release.** There is no such release
  to upgrade to. `rav1e` is at `0.8.1` and still declares `paste ^1.0`.
  `exr 1.74.3` dropped `pulp` entirely, but `exr` only participates because
  `default-formats` enables `image`'s `exr` feature; a lockfile bump would
  still leave `ravif -> rav1e -> paste` in place unless `avif` were abandoned.
- **Suppress the advisory.** `cargo audit` allow-list entries, `--ignore`
  flags and a `.cargo/audit.toml` would all hide a real maintenance finding.
  The task forbids it and none was added.
- **Upgrade or replace `openai-harmony`.** `0.0.8` is its newest published
  version. Switching to a mirror crate would change a runtime dependency for
  reasons unrelated to this advisory.

## Codec retention

JPEG, PNG, GIF and WebP support is preserved because none of those decoders was
on the advisory path, and every declared format is proven by a test that
exercises a real file of that format.

- The root declaration still requests `jpeg`, `png`, `gif` and `webp`.
- `cargo tree --locked -e features -i image` after the change shows exactly
  those four features, with `diapason-ui` as the activator for each and
  `arboard` adding `png` through `image-data`.
- `decodes_png_file_path_image_as_png`, `decodes_jpeg_file_path_image_as_png`,
  `decodes_gif_file_path_image_as_png` and
  `decodes_webp_file_path_image_as_png` in
  `crates/codegen/diapason-ui/src/tui/core_tui/session/clipboard_image.rs`
  write a deterministic sample image in each format, pass it through the
  clipboard file-path path, and assert the returned content part is `image/png`
  that decodes to the original geometry. The lossless formats additionally
  compare pixels; JPEG is compared by geometry only.
- AVIF and OpenEXR were never declared formats for Diapason. They arrived
  exclusively through `openai-harmony`'s unwanted defaults, so removing them
  changes no supported user behaviour.

## Verification

- `cargo audit --file Cargo.lock` reports no `RUSTSEC-2024-0436` and no new
  advisory identifier.
- `cargo tree --locked -i paste` reports no match.
- `cargo metadata` resolves without a "patch was not used" warning.
- Locked `cargo check`/`clippy` with `RUSTFLAGS="-D warnings"`, the
  `diapason-ui` clipboard nextest filter, the `diapason-llm` Harmony tests and
  the repository gates run clean.

## First-party policy gates and vendored code

Vendoring registry sources puts third-party Rust under `patches/`, where the
first-party gates would otherwise judge it. Two gates needed a scope decision,
and in both cases the exclusion names the vendored tree rather than silencing a
finding about Diapason:

- `scripts/check_no_unwrap_expect_prod.py` gained a repeatable `--exclude`
  glob option. Both `make advisory` and the CI `unwrap-expect-enforcement` job
  pass `--exclude 'patches/*/src/**'`. The CI enforce-mode job previously
  failed on 14 upstream `unwrap` calls in the vendored crate; rewriting them
  would put the vendor tree out of sync with the released archive for no
  behavioural gain, which is exactly what the patch refuses to do everywhere
  else. First-party scanning is unchanged: the run still covers 2000 files and
  reports zero findings.
- `mdtablefix` reflows every tracked Markdown file, so the vendored `README.md`
  is reflowed as described above. No other gate flags the vendored tree.
  `scripts/check_rust_file_length.py` lists `patches/simdutf/src/generated.rs`
  in warn mode only, which predates this work, and the large-file gate needed
  no new allowlist entry.

This is not an advisory allow-list. `RUSTSEC-2024-0436` is gone because the
dependency left the graph, and nothing in this change suppresses a RustSec
warning.

## Publication limitation

`[patch.crates-io]` entries are local. They apply to this workspace and never
reach consumers of a published crate. A downstream project that depends on a
published `diapason-llm` resolves `openai-harmony` from crates.io, still gets
its `image` defaults, and still gets `paste` on an AVIF-enabled build.

That limitation is inherent to a workspace-level patch and is why the vendored
copy records an exit criterion rather than a permanent fork. Until upstream
releases the fix, the patch is correct for Diapason and does not claim to fix
its consumers. `cargo publish --dry-run -p diapason-llm` validates against the
registry `openai-harmony`, so packaging is unaffected by the patch.

## Exit criterion

When a released `openai-harmony` drops `image` or disables its default features:

1. bump the `diapason-llm` requirement to the fixed version,
2. delete `patches/openai-harmony`,
3. remove the `openai-harmony` entry from `[patch.crates-io]` and from
   `workspace.exclude` in the root `Cargo.toml`,
4. regenerate `Cargo.lock`,
5. confirm `cargo tree --locked -i paste` stays empty and
   `cargo audit --file Cargo.lock` still reports no `RUSTSEC-2024-0436`.

## Outcomes & retrospective

The advisory was not a `diapason-ui` image configuration problem. The
declaration was already as narrow as the declared formats allow. The activator
sat one dependency layer away, in a crate that declared an image dependency it
never used, and Cargo's additive feature model turned that dead declaration
into four extra codec backends for the whole workspace. Lockfile surgery could
not have helped: only removing the declaration changed the resolution.
