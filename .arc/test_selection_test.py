"""Claims: unknown or dangerous deltas run the full suite; only Markdown skips work."""

import contextlib
import importlib.util
import io
import json
from pathlib import Path
import re
import subprocess
import sys
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location(
    "selection", Path(__file__).with_name("test-selection.py")
)
selection = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(selection)


class SelectionClaims(unittest.TestCase):
    def test_rejects_narrowing_when_it_cannot_classify(self):
        for paths in (
            None,
            [],
            ["unknown"],
            ["README.md", "build.rs"],
            ["Cargo.toml"],
            ["Cargo.lock"],
            [".arc/test-selection.py"],
            ["tests/cli/common.rs"],
            ["tests/cli/main.rs"],
            ["tests/cli/missing.rs"],
            ["src/commands/missing.rs"],
            ["src/gates.rs"],
            [".arc/gates.toml"],
            [None],
            [""],
        ):
            with self.subTest(paths=paths):
                self.assertEqual(selection.select(paths), ("full", [], False))

    def test_only_nonempty_markdown_deltas_select_nothing(self):
        self.assertEqual(
            selection.select(["README.md", "docs/notes.md"]), ("markdown", [], False)
        )

    def test_cli_modules_run_without_unrelated_unit_tests(self):
        self.assertEqual(
            selection.select(["tests/cli/diff.rs"]), ("modules", ["diff"], False)
        )

    def test_matching_sources_include_units_and_union_modules(self):
        paths = [
            "src/commands/diff.rs",
            "src/commands/cost.rs",
            "tests/cli/diff.rs",
            "README.md",
        ]
        self.assertEqual(
            selection.select(paths), ("modules", ["cost", "diff"], True)
        )

    def test_guide_source_selects_every_module_that_reads_guide_text(self):
        readers = sorted(
            path.stem
            for path in Path("tests/cli").glob("*.rs")
            if path.stem not in ("main", "common")
            and re.search(r'instruction_topics|"guide"', path.read_text())
        )
        mode, modules, units = selection.select(["src/guide.rs"])
        self.assertEqual((mode, units), ("modules", True))
        self.assertEqual(sorted(set(readers) - set(modules)), [])

    def test_deleted_or_unregistered_modules_require_the_full_suite(self):
        with patch.object(selection.Path, "is_file", return_value=False):
            self.assertEqual(
                selection.select(["tests/cli/diff.rs"]), ("full", [], False)
            )
        with patch.object(selection.Path, "read_text", return_value=""):
            self.assertEqual(
                selection.select(["tests/cli/diff.rs"]), ("full", [], False)
            )

    def test_diff_failure_requires_the_full_suite(self):
        with patch.object(
            selection, "git", side_effect=subprocess.CalledProcessError(1, "git")
        ):
            self.assertEqual(selection.select(selection.delta()), ("full", [], False))

    def test_diff_names_both_rename_paths_and_uses_the_resolved_target(self):
        with patch.object(
            selection, "git", side_effect=["target", "base", "old.rs\0new.md\0"]
        ) as git:
            self.assertEqual(selection.delta(), ["old.rs", "new.md"])
            self.assertEqual(
                git.call_args_list[1].args, ("merge-base", "target", "HEAD")
            )
            self.assertEqual(
                git.call_args.args,
                ("diff", "--no-renames", "--name-only", "-z", "base", "HEAD", "--"),
            )

    def test_selector_excludes_every_gate_only_on_markdown(self):
        gates = ["build", "lint", "test-integration"]
        for paths, affected in (
            (["README.md"], []),
            ([], gates),
            (["unknown"], gates),
        ):
            request = {
                "schema": "arc-gate-delta/1",
                "source_tree": "before",
                "target_tree": "after",
                "paths": paths,
                "gates": gates,
            }
            output = io.StringIO()
            with patch(
                "sys.stdin", io.StringIO(json.dumps(request))
            ), contextlib.redirect_stdout(output):
                selection.selector()
            answer = json.loads(output.getvalue())
            self.assertEqual(answer["schema"], "arc-gate-selection/1")
            self.assertTrue(answer["complete"])
            self.assertEqual(answer["request"], request)
            self.assertEqual(answer["gates"], affected)

    def test_a_dangerous_path_runs_the_full_suite_beside_a_matching_module(self):
        self.assertIn("journal", re.findall(r"^mod (\w+);$", Path("tests/cli/main.rs").read_text(), re.M))
        self.assertEqual(selection.select(["src/journal.rs"]), ("full", [], False))
        self.assertEqual(
            selection.select(["src/journal.rs", "tests/cli/diff.rs"]), ("full", [], False)
        )

    def test_unreadable_danger_declarations_run_the_full_suite(self):
        with patch.object(selection, "danger_paths", return_value=None):
            self.assertEqual(selection.select(["tests/cli/diff.rs"]), ("full", [], False))

    def test_only_a_markdown_delta_skips_a_gate_command(self):
        for paths, runs in ((["README.md"], False), (["tests/cli/diff.rs"], True), (None, True)):
            with patch.object(selection, "delta", return_value=paths), patch.object(
                sys, "argv", ["selection", "--unless-markdown", "false"]
            ), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(selection.main(), 1 if runs else 0)

    def test_malformed_selector_input_proves_no_exclusion(self):
        for raw in ("{}", "null", "[]", "broken"):
            output = io.StringIO()
            with patch("sys.stdin", io.StringIO(raw)), contextlib.redirect_stdout(
                output
            ):
                selection.selector()
            self.assertFalse(json.loads(output.getvalue())["complete"])

    def test_target_movement_changes_the_environment_identity(self):
        identities = []
        for target in ("before", "after"):
            output = io.StringIO()
            with patch.object(selection, "git", return_value=target), patch.object(
                selection.subprocess, "check_output", return_value="toolchain"
            ), contextlib.redirect_stdout(output):
                selection.environment()
            identities.append(output.getvalue())
        self.assertNotEqual(*identities)

    def test_empty_or_failed_module_inventory_runs_cargo_test(self):
        for result in (
            subprocess.CompletedProcess([], 0, "", ""),
            subprocess.CompletedProcess([], 1, "", "error"),
        ):
            with patch.object(
                selection.subprocess,
                "run",
                side_effect=[
                    result,
                    subprocess.CompletedProcess([], 0),
                    subprocess.CompletedProcess([], 0),
                ],
            ) as run:
                self.assertEqual(selection.run(("modules", ["diff"], False)), 0)
                self.assertEqual(run.call_args.args[0], ["cargo", "test"])

    def test_source_selection_runs_units_and_cli_and_preserves_failure(self):
        results = [
            subprocess.CompletedProcess([], 0, "diff::a: test\n", ""),
            subprocess.CompletedProcess([], 0),
            subprocess.CompletedProcess([], 7),
        ]
        with patch.object(selection.subprocess, "run", side_effect=results) as run:
            self.assertEqual(selection.run(("modules", ["diff"], True)), 7)
            self.assertEqual(
                run.call_args_list[1].args[0], ["cargo", "test", "--bin", "arc"]
            )
            self.assertEqual(
                run.call_args_list[2].args[0],
                ["cargo", "test", "--test", "cli", "--", "diff::"],
            )


if __name__ == "__main__":
    unittest.main()
