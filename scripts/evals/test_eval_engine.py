"""Unit tests for the evaluation engine command construction."""
import os
import json
import sys
import unittest

sys.path.insert(0, os.path.dirname(__file__))

from eval_engine import EvaluationEngine


class EvaluationEngineCommandTests(unittest.TestCase):
    """Tests for the diapason command built by EvaluationEngine."""
    def setUp(self):
        """Create an engine with test provider and model settings."""
        self.engine = EvaluationEngine(provider="test-provider", model="test-model")

    def test_advanced_profile_is_explicit(self):
        """Check the advanced profile is set explicitly and exec args are kept."""
        command = self.engine.build_command(
            "find symbols", "advanced_diapason", "/tmp/final-message.txt"
        )

        self.assertIn("-c", command)
        self.assertIn("tools.profile=advanced_diapason", command)
        self.assertEqual(
            command[-5:],
            [
                "exec",
                "--json",
                "--last-message-file",
                "/tmp/final-message.txt",
                "find symbols",
            ],
        )

    def test_default_profile_is_explicit(self):
        """Check the default diapason profile is set as an explicit override."""
        command = self.engine.build_command(
            "list files", "diapason", "/tmp/final-message.txt"
        )

        self.assertIn("tools.profile=diapason", command)

    def test_unknown_profile_is_rejected(self):
        """Check that an unsupported eval profile raises ValueError."""
        with self.assertRaisesRegex(ValueError, "Unsupported eval profile"):
            self.engine.build_command(
                "archival case", "archived_baseline", "/tmp/final-message.txt"
            )

    def test_ai_tool_surface_suite_has_only_executable_profiles(self):
        """Check AI tool-surface eval cases use only executable profiles."""
        cases_path = os.path.join(
            os.path.dirname(__file__),
            "..",
            "docs",
            "development",
            "ai-tool-surface-eval-cases.json",
        )
        with open(cases_path, encoding="utf-8") as cases_file:
            cases = json.load(cases_file)

        self.assertEqual(len(cases), 7)
        self.assertEqual(
            {case["profile"] for case in cases},
            {"diapason", "advanced_diapason"},
        )
        self.assertNotIn(
            "tool_surface_archived_baseline_unavailable",
            {case["id"] for case in cases},
        )
        self.assertEqual(
            [case["metric"] for case in cases[:2]],
            ["llm_grader", "llm_grader"],
        )
        self.assertTrue(
            all(case["metric"] in self.engine.metrics for case in cases)
        )


if __name__ == "__main__":
    unittest.main()
