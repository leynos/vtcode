#!/usr/bin/env python3
"""Declared header boundaries retain test scope and resume production scanning."""

from __future__ import annotations

import os
import unittest
from unittest.mock import patch

from test_check_no_unwrap_expect_prod import CHECKER, ScannerHarness


class AttributedScopeTests(ScannerHarness):
    def _assert_block_item(self, header: str, *, attribute: str = "cfg(test)") -> None:
        source = (
            f"#[{attribute}]\n{header} {{\n"
            'fn inner() { value.expect("test body"); }\n}\n'
            'fn production() { value.expect("production"); }\n'
        )
        path = self.rust("src/lib.rs", source)
        findings = CHECKER.scan_file(path, [])
        self.assertEqual([line for _, line, _ in findings], [source.count("\n")])

    def test_builtin_test_result_return_keeps_its_body_test_only(self) -> None:
        source = (
            '#[test]\nfn check() -> Result<(), String> {\n'
            'value.expect("test");\nOk(())\n}\n'
            'fn production() { value.unwrap(); }\n'
        )
        path = self.rust("src/lib.rs", source)

        self.assertEqual(CHECKER.RustSource.read(path).production_call_lines(), [6])

    def test_qualified_generic_and_where_headers_keep_their_balanced_body(self) -> None:
        headers = (
            "pub(crate) async fn helper<T, U>() -> Result<Vec<(T, U)>, String> "
            "where T: Clone, U: Default",
            'pub(in crate::nested) const unsafe extern "C" fn helper<T, U>() '
            "-> Result<T, U>",
            "default unsafe impl<T, U> Trait<(T, U)> for Container<T, U> "
            "where T: Clone, U: Copy",
            "impl<const N: usize, T> Container<{ N + 1 }, T> "
            "where [u8; { N + 1 }]: Sized, T: Copy",
            "pub trait Convert<T, U> where T: Clone, U: Copy",
        )
        for header in headers:
            with self.subTest(header=header):
                self._assert_block_item(header)

    def test_const_expression_in_generic_header_is_not_the_function_body(self) -> None:
        self._assert_block_item(
            "fn helper<const N: usize>() -> Result<Container<{ N + 1 }, (u8, u16)>, String>"
        )

    def test_const_default_inside_struct_header_remains_test_only(self) -> None:
        source = (
            '#[cfg(test)]\nstruct Buffer<const N: usize = { value.expect("default") }, '
            'T = (u8, u16)> { data: [T; N] }\n'
            'fn production() { value.unwrap(); }\n'
        )
        path = self.rust("src/lib.rs", source)

        self.assertEqual(CHECKER.RustSource.read(path).production_call_lines(), [3])

    def test_const_and_static_initializers_continue_after_their_inner_block(self) -> None:
        for keyword in ("const", "static"):
            with self.subTest(keyword=keyword):
                source = (
                    f'#[cfg(test)]\n{keyword} VALUE: Result<(), String> = '
                    '{ value }.expect("test initializer");\n'
                    'fn production() { value.unwrap(); }\n'
                )
                path = self.rust("src/lib.rs", source)
                self.assertEqual(CHECKER.RustSource.read(path).production_call_lines(), [3])

    def test_match_guard_comparison_does_not_consume_the_next_arm(self) -> None:
        source = (
            'fn record() { match self {\n#[cfg(test)]\n'
            'Self::Test if count < 2 => value.expect("test"),\n'
            'Self::Production => value.expect("production"),\n} }\n'
        )
        path = self.rust("src/lib.rs", source)

        self.assertEqual(CHECKER.RustSource.read(path).production_call_lines(), [4])

    def test_attributed_statement_and_field_keep_their_own_comma_boundaries(self) -> None:
        statements = (
            'fn f() { #[cfg(test)] let x = (value.unwrap(), other);\n'
            'value.expect("production"); }\n',
            'struct S { #[cfg(test)] test: [(); { value.unwrap(); 0 }],\n'
            'production: [(); { value.expect("production"); 0 }], }\n',
        )
        for source in statements:
            with self.subTest(source=source):
                path = self.rust("src/lib.rs", source)
                self.assertEqual(CHECKER.RustSource.read(path).production_call_lines(), [2])

    def test_unterminated_generic_header_establishes_no_test_proof(self) -> None:
        source = (
            '#[cfg(test)]\nfn helper<T, U() { value.expect("unproven"); }\n'
            'fn production() { value.unwrap(); }\n'
        )
        path = self.rust("src/lib.rs", source)

        self.assertEqual(CHECKER.RustSource.read(path).production_call_lines(), [2, 3])

    @unittest.skipIf(os.name == "nt", "Requires POSIX directory-symlink semantics.")
    def test_module_graph_preserves_policy_under_a_symlink_root_spelling(self) -> None:
        canonical = self.root / "canonical"
        canonical.mkdir()
        alias = self.root / "alias"
        alias.symlink_to(canonical, target_is_directory=True)
        root_file = alias / "lib.rs"
        helper_file = alias / "helper.rs"
        root_file.write_text("#[cfg(test)] mod helper;\n", encoding="utf-8")
        helper_file.write_text('fn check() { value.expect("test"); }\n', encoding="utf-8")

        with patch.object(CHECKER, "REPO_ROOT", alias):
            excluded = CHECKER.test_only_files([root_file, helper_file])

        self.assertEqual(excluded, {helper_file.resolve()})
