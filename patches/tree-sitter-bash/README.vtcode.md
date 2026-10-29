# VTCode scanner backport

This directory retains the complete published `tree-sitter-bash` 0.25.1
package, including its MIT licence and generated parser. Its source is the
[official upstream repository](https://github.com/tree-sitter/tree-sitter-bash).

The
[published crate archive](https://static.crates.io/crates/tree-sitter-bash/tree-sitter-bash-0.25.1.crate)
has SHA-256:

```text
9e5ec769279cc91b561d3df0d8a5deb26b0ad40d183127f409494d6d8fc53062
```

The archive's `.cargo_vcs_info.json` records upstream source commit
`a06c2e4415e9bc0346c6b86d401879ffb44058f7`. Registry cache markers such as
`.cargo-ok` are not included.

The scanner source change backports the fix from
[upstream PR #348](https://github.com/tree-sitter/tree-sitter-bash/pull/348),
reviewed at immutable head
[`484491f8bfe1903e2c97ba9f60bc8ee83ccba25e`](https://github.com/tree-sitter/tree-sitter-bash/pull/348/commits/484491f8bfe1903e2c97ba9f60bc8ee83ccba25e):
replace both `isdigit(lexer->lookahead)` calls with `iswdigit` and remove the
unused `<ctype.h>` include. The scanner already uses `<wctype.h>`. Narrow
`isdigit` is undefined for Unicode lookahead values outside the unsigned-byte
range, apart from `EOF`.

The second source-code adaptation replaces the two `to_str().unwrap()` calls in
`bindings/rust/build.rs` with `Path::display()` for VTCode's unwrap policy. The
paths are fixed relative ASCII names, `src/parser.c` and `src/scanner.c`, so
the Cargo rerun directives and C build inputs remain unchanged.

Repository formatting may normalize published Markdown without changing the
scanner backport.

No fixed release was available when this backport was added. A shared
`[patch.crates-io]` override applies it to both parser consumers without a
contributor-fork dependency. The exact workspace exclusion keeps this upstream
package outside VTCode's product workspace. Its build script compiles
`src/parser.c`, so the 9,912,254-byte generated file is retained unchanged with
an exact-path size allowance. Regenerating or trimming it would expand this
native scanner backport.

Remove this directory, its path override, workspace exclusion and size
allowance once a compatible published release includes the fix and passes the
repository gates. Restore the registry package identity in `Cargo.lock`.

The original CI fuzz SIGSEGV did not reproduce on Rocky Linux in the seeded or
direct normal test runs. This backport removes the identified scanner defect;
its role in that crash remains unproven, and no failing baseline is claimed.
Validation requires a coverage-instrumented replay with
`PROPTEST_RNG_SEED=3441062684832341064` plus the repository gates.
