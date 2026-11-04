"""Integration coverage for documentation-map archive exclusions."""

import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
GENERATOR = REPO_ROOT / "scripts" / "generate_docs_map.py"


class GenerateDocsMapArchiveExclusionTests(unittest.TestCase):
    def test_cli_excludes_archive_subtree_and_keeps_similarly_named_docs(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            fixture_root = Path(temporary_directory)
            fixture_scripts = fixture_root / "scripts"
            fixture_docs = fixture_root / "docs"
            fixture_scripts.mkdir()
            (fixture_docs / "modules").mkdir(parents=True)
            shutil.copy2(GENERATOR, fixture_scripts / GENERATOR.name)

            documents = {
                "archive/analysis/archived-report.md": (
                    "# Archive Only Title Marker\n\n"
                    "## Archive Only Topic Marker\n"
                ),
                "guides/current-guide.md": "# Current Maintained Guide\n",
                "archive-guide.md": "# Archive Guide Sibling Document\n",
                "archive-guides/parallel-guide.md": (
                    "# Archive Guides Sibling Directory\n"
                ),
            }
            for relative_path, content in documents.items():
                document = fixture_docs / relative_path
                document.parent.mkdir(parents=True, exist_ok=True)
                document.write_text(content, encoding="utf-8")

            subprocess.run(
                [sys.executable, str(fixture_scripts / GENERATOR.name)],
                cwd=fixture_root,
                check=True,
                capture_output=True,
                text=True,
            )

            generated_map = fixture_docs / "modules" / "diapason_docs_map.md"
            map_content = generated_map.read_text(encoding="utf-8")

            self.assertNotIn("docs/archive/analysis/archived-report.md", map_content)
            self.assertNotIn("Archive Only Title Marker", map_content)
            self.assertNotIn("Archive Only Topic Marker", map_content)
            self.assertIn("docs/guides/current-guide.md", map_content)
            self.assertIn("docs/archive-guide.md", map_content)
            self.assertIn("docs/archive-guides/parallel-guide.md", map_content)


if __name__ == "__main__":
    unittest.main()
