#!/usr/bin/env python3
"""Production enforcement and test-context regressions for the Rust scanner."""

from __future__ import annotations

from contextlib import redirect_stderr, redirect_stdout
import importlib.util
from io import StringIO
from pathlib import Path
import sys
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch


SCRIPT_PATH = Path(__file__).resolve().parents[1] / "check_no_unwrap_expect_prod.py"
SPEC = importlib.util.spec_from_file_location("check_no_unwrap_expect_prod", SCRIPT_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"Unable to import {SCRIPT_PATH}")
CHECKER = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = CHECKER
SPEC.loader.exec_module(CHECKER)


class ScannerHarness(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.root_patch = patch.object(CHECKER, "REPO_ROOT", self.root)
        self.root_patch.start()
        self.addCleanup(self.root_patch.stop)
        self.files: list[Path] = []

    def rust(self, name: str, source: str) -> Path:
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(source, encoding="utf-8")
        self.files.append(path)
        return path

    def findings(self) -> list[tuple[str, int]]:
        excluded = CHECKER.test_only_files(self.files)
        return [
            (name, line)
            for path in self.files
            if not CHECKER.is_test_path(path) and path.resolve() not in excluded
            for name, line, _ in CHECKER.scan_file(path, [])
        ]


class ProductionScannerTests(ScannerHarness):
    def test_production_expect_is_reported_with_its_source_line(self) -> None:
        self.rust("src/lib.rs", 'fn production() {\n    value.expect("required");\n}\n')

        self.assertEqual(self.findings(), [("src/lib.rs", 2)])

    def test_recursive_path_modules_inherit_explicit_test_context(self) -> None:
        self.rust("src/lib.rs", '#[cfg(test)]\n#[path = "responses_tests.rs"]\nmod responses;\n')
        self.rust("src/responses_tests.rs", '#[path = "responses_harness.rs"]\nmod harness;\n')
        self.rust("src/responses_harness.rs", 'fn capture() { value.expect("test capture"); }\n')

        self.assertEqual(self.findings(), [])

    def test_default_external_module_inherits_test_context(self) -> None:
        self.rust("src/lib.rs", '#[cfg(test)]\nmod support;\n')
        self.rust("src/support.rs", 'mod helper;\n')
        self.rust("src/support/helper.rs", 'fn check() { value.unwrap(); }\n')

        self.assertEqual(self.findings(), [])

    def test_production_reference_overrides_test_only_external_owner(self) -> None:
        self.rust(
            "src/lib.rs",
            '#[cfg(test)]\n#[path = "support.rs"]\nmod test_support;\n'
            '#[path = "support.rs"]\nmod production_support;\n',
        )
        self.rust("src/support.rs", 'fn check() { value.expect("shared"); }\n')

        self.assertEqual(self.findings(), [("src/support.rs", 1)])

    def test_shared_parent_keeps_recursive_child_enforced(self) -> None:
        self.rust(
            "src/lib.rs",
            '#[cfg(test)]\n#[path = "shared.rs"]\nmod tests;\n'
            '#[path = "shared.rs"]\nmod shared;\n',
        )
        self.rust("src/shared.rs", '#[path = "child.rs"] mod child;\n')
        self.rust("src/child.rs", 'fn check() { value.unwrap(); }\n')

        self.assertEqual(self.findings(), [("src/child.rs", 1)])

    def test_test_impl_and_inline_module_resume_at_following_production(self) -> None:
        self.rust(
            "src/lib.rs",
            '#[cfg(test)]\nimpl Helper { fn check() { value.unwrap(); } }\n'
            '#[cfg(test)]\nmod tests { fn check() { value.expect("test"); } }\n'
            'fn production() { value.expect("production"); }\n',
        )

        self.assertEqual(self.findings(), [("src/lib.rs", 5)])

    def test_test_match_arm_does_not_hide_following_production_arm(self) -> None:
        self.rust(
            "src/lib.rs",
            'fn record() { match self {\n'
            '#[cfg(test)]\nSelf::Test(recorder) => { recorder.expect("test"); }\n'
            'Self::Production => value.expect("production"),\n} }\n',
        )

        self.assertEqual(self.findings(), [("src/lib.rs", 4)])

    def test_test_attribute_handles_multiline_function_and_balanced_literals(self) -> None:
        self.rust(
            "src/lib.rs",
            '#[tokio::test]\nasync fn check(\n) {\n'
            'let braces = r###"} #[cfg(test)] {"###;\n'
            'let quoted = "} \\\" {"; /* } /* nested { */ } */\n'
            "let brace = '}';\nvalue.expect(\"test\");\n}\n"
            'fn production() { value.unwrap(); }\n',
        )

        self.assertEqual(self.findings(), [("src/lib.rs", 9)])

    def test_non_test_and_mixed_cfg_conditions_stay_enforced(self) -> None:
        for condition in ('not(test)', 'any(test, feature = "extra")', 'feature = "test"'):
            with self.subTest(condition=condition):
                path = self.rust("src/lib.rs", f'#[cfg({condition})]\nfn check() {{ value.unwrap(); }}\n')
                self.assertEqual(CHECKER.scan_file(path, [])[0][:2], ("src/lib.rs", 2))

    def test_unknown_namespaced_test_macro_remains_enforced(self) -> None:
        self.rust(
            "src/lib.rs",
            '#[custom::test]\nfn production() { value.expect("required"); }\n',
        )

        self.assertEqual(self.findings(), [("src/lib.rs", 2)])

    def test_builtin_and_supported_tokio_attributes_prove_test_context(self) -> None:
        for attribute in ("test", "tokio::test", 'tokio::test(flavor = "current_thread")'):
            with self.subTest(attribute=attribute):
                function_kind = "fn" if attribute == "test" else "async fn"
                path = self.rust(
                    "src/lib.rs",
                    f'#[{attribute}]\n{function_kind} check() {{ value.expect("test"); }}\n',
                )
                self.assertEqual(CHECKER.scan_file(path, []), [])

    def test_ungated_module_named_tests_is_still_production_code(self) -> None:
        self.rust("src/lib.rs", 'mod tests { fn production() { value.unwrap(); } }\n')

        self.assertEqual(self.findings(), [("src/lib.rs", 1)])

    def test_inner_attribute_does_not_extend_following_test_function_scope(self) -> None:
        self.rust(
            "src/lib.rs",
            '#![allow(dead_code)]\n#[cfg(test)]\n'
            'fn check() { value.unwrap(); }\n'
            'fn production() { value.expect("required"); }\n',
        )

        self.assertEqual(self.findings(), [("src/lib.rs", 4)])

    def test_same_line_test_block_does_not_hide_production_after_it(self) -> None:
        self.rust(
            "src/lib.rs",
            '#[cfg(test)] mod tests { fn t() { value.unwrap(); } } '
            'fn production() { value.expect("required"); }\n',
        )

        self.assertEqual(self.findings(), [("src/lib.rs", 1)])

    def test_comments_and_strings_cannot_create_a_test_context_or_finding(self) -> None:
        self.rust(
            "src/lib.rs",
            '// #[cfg(test)]\n/* #[cfg(test)] */\n'
            'const TEXT: &str = r#"#[cfg(test)] mod tests { value.unwrap(); }"#;\n'
            'fn production() { value.expect("// braces } .unwrap()"); }\n',
        )

        self.assertEqual(self.findings(), [("src/lib.rs", 4)])

    def test_unresolved_external_path_does_not_exclude_unowned_file(self) -> None:
        self.rust("src/lib.rs", '#[cfg(test)]\n#[path = "missing.rs"]\nmod helper;\n')
        self.rust("src/helper.rs", 'fn production() { value.expect("unresolved owner"); }\n')

        self.assertEqual(self.findings(), [("src/helper.rs", 1)])

    def test_ambiguous_cfg_path_keeps_possible_production_owner_enforced(self) -> None:
        self.rust(
            "src/lib.rs",
            '#[cfg(test)]\n#[path = "support.rs"]\nmod test_support;\n'
            '#[cfg_attr(feature = "extra", path = "support.rs")]\nmod shared;\n',
        )
        self.rust("src/support.rs", 'fn check() { value.unwrap(); }\n')

        self.assertEqual(self.findings(), [("src/support.rs", 1)])

    def test_ambiguous_path_inside_test_parent_stays_enforced(self) -> None:
        self.rust("src/lib.rs", '#[cfg(test)] mod support;\n')
        self.rust(
            "src/support.rs",
            '#[cfg_attr(feature = "extra", path = "helper.rs")] mod helper;\n',
        )
        self.rust("src/helper.rs", 'fn check() { value.unwrap(); }\n')

        self.assertEqual(self.findings(), [("src/helper.rs", 1)])

    def test_unresolved_escaped_path_cannot_hide_shared_production_file(self) -> None:
        self.rust(
            "src/lib.rs",
            '#[cfg(test)]\n#[path = "support.rs"] mod test_support;\n'
            '#[path = "supp\\u{006f}rt.rs"] mod shared;\n',
        )
        self.rust("src/support.rs", 'fn check() { value.unwrap(); }\n')

        self.assertEqual(self.findings(), [("src/support.rs", 1)])

    def test_production_include_keeps_shared_file_enforced(self) -> None:
        self.rust(
            "src/lib.rs",
            '#[cfg(test)]\n#[path = "support.rs"] mod test_support;\n'
            'include!("support.rs");\n',
        )
        self.rust("src/support.rs", 'fn check() { value.unwrap(); }\n')

        self.assertEqual(self.findings(), [("src/support.rs", 1)])

    def test_unbalanced_scope_does_not_prove_test_only_code(self) -> None:
        self.rust("src/lib.rs", '#[cfg(test)]\nmod tests { value.unwrap();\n')

        self.assertEqual(self.findings(), [("src/lib.rs", 2)])

    def test_inline_test_module_resolves_nested_external_path(self) -> None:
        self.rust("src/lib.rs", '#[cfg(test)]\nmod tests { #[path = "capture.rs"] mod capture; }\n')
        self.rust("src/tests/capture.rs", 'fn capture() { value.unwrap(); }\n')

        self.assertEqual(CHECKER.test_only_files(self.files), {self.files[1].resolve()})

    def test_filename_policy_and_line_allowlist_remain_supported(self) -> None:
        self.rust("src/helper_tests.rs", 'fn check() { value.unwrap(); }\n')
        production = self.rust("src/lib.rs", 'fn a() { value.unwrap(); }\nfn b() { value.expect("required"); }\n')
        rules = [CHECKER.AllowRule("src/lib.rs", line=1)]

        self.assertEqual(
            [(name, line) for name, line, _ in CHECKER.scan_file(production, rules)],
            [("src/lib.rs", 2)],
        )
        self.assertTrue(CHECKER.is_test_path(self.files[0]))

    def test_warn_and_enforce_exit_statuses_keep_production_control(self) -> None:
        self.rust("src/lib.rs", 'fn production() { value.expect("required"); }\n')
        for mode, expected in (("warn", 0), ("enforce", 1)):
            with self.subTest(mode=mode):
                output = StringIO()
                with patch.object(CHECKER, "list_tracked_rust_files", return_value=self.files):
                    with patch.object(sys, "argv", [str(SCRIPT_PATH), "--mode", mode]):
                        with redirect_stdout(output), redirect_stderr(StringIO()):
                            status = CHECKER.main()
                self.assertEqual(status, expected)
                self.assertIn("findings=1", output.getvalue())

    def test_enforce_succeeds_for_only_explicit_test_contexts(self) -> None:
        self.rust("src/lib.rs", '#[cfg(test)]\n#[path = "support.rs"] mod support;\n')
        self.rust("src/support.rs", 'fn check() { value.expect("test"); }\n')
        with patch.object(CHECKER, "list_tracked_rust_files", return_value=self.files):
            with patch.object(sys, "argv", [str(SCRIPT_PATH), "--mode", "enforce"]):
                with redirect_stdout(StringIO()), redirect_stderr(StringIO()):
                    status = CHECKER.main()

        self.assertEqual(status, 0)


class TestPathClassification(unittest.TestCase):
    def test_nested_test_support_module_is_excluded(self) -> None:
        support_path = (
            CHECKER.REPO_ROOT
            / "crates/codegen/vtcode-acp/src/zed/agent/tool_config_tests/support.rs"
        )

        self.assertTrue(CHECKER.is_test_path(support_path))

    def test_production_rust_module_remains_scanned(self) -> None:
        production_path = (
            CHECKER.REPO_ROOT
            / "crates/codegen/vtcode-acp/src/zed/provider_runtime.rs"
        )

        self.assertFalse(CHECKER.is_test_path(production_path))

    def test_contests_module_is_not_mistaken_for_test_source(self) -> None:
        production_path = CHECKER.REPO_ROOT / "crates/example/src/contests.rs"

        self.assertFalse(CHECKER.is_test_path(production_path))


if __name__ == "__main__":
    unittest.main()
