"""Acceptance claims for the exact-name artifact runner; run on demand."""

import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location("runner", Path(__file__).with_name("test-binary.py"))
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)


class RunnerClaims(unittest.TestCase):
    def test_missing_exact_name_refuses_before_running(self):
        """Claim: a renamed witness fails; rejects substring or absent-name acceptance."""
        rows = [{"target": "cli", "test": "area::witness"}]
        with self.assertRaisesRegex(ValueError, "missing exact witnesses"):
            runner.validate(rows, {"cli": {"area::witness_renamed"}})

    def test_duplicate_or_empty_list_refuses(self):
        """Claim: selection is explicit and unique; rejects an empty or duplicated gate."""
        row = "I5\tcli\tarea::witness\tReject an unauthorized effect\n"
        for body in ["", row + row]:
            with self.assertRaises(ValueError):
                runner.witnesses(io.StringIO("invariant\ttarget\ttest\tclaim\n" + body))

    def test_exact_dispatch_preserves_failure_and_selected_artifact(self):
        """Claim: exact dispatch propagates failure using ARC_TEST_BIN; rejects false green."""
        env = {"ARC_TEST_BIN": "/selected/arc"}
        with patch.object(runner.subprocess, "run", return_value=subprocess.CompletedProcess([], 7, "test result: ok. 1 passed; 0 failed; 0 ignored;")) as run:
            result = runner.run_one("/harness", "area::witness", env)
        self.assertFalse(result[2])
        self.assertEqual(run.call_args.args[0], ["/harness", "--exact", "area::witness", "--nocapture"])
        self.assertIs(run.call_args.kwargs["env"], env)

    def test_zero_or_ignored_execution_refuses(self):
        """Claim: success requires one executed witness; rejects empty or ignored runs."""
        for output in ["0 passed; 0 failed; 0 ignored;", "0 passed; 0 failed; 1 ignored;"]:
            with patch.object(runner.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, output)):
                self.assertFalse(runner.run_one("/harness", "area::witness", {})[2])

    def test_artifact_path_comes_from_cargo_receipt(self):
        """Claim: the release check uses Cargo's executable; rejects a guessed stale path."""
        receipt = {"reason": "compiler-artifact", "target": {"name": "arc"},
                   "profile": {"test": False}, "executable": "/selected/release/arc"}
        build = subprocess.CompletedProcess([], 0, json.dumps(receipt))
        with patch.object(runner.subprocess, "run", return_value=build):
            self.assertEqual(runner.cargo_artifacts(["cargo"])["arc"], "/selected/release/arc")

    def test_failed_build_shows_compiler_messages_and_refuses(self):
        """Claim: a build failure prints rustc's message and refuses; rejects a silent or green failed build."""
        message = {"reason": "compiler-message", "message": {"rendered": "error[E0425]: missing\n"}}
        build = subprocess.CompletedProcess([], 101, json.dumps(message))
        shown = io.StringIO()
        with patch.object(runner.subprocess, "run", return_value=build), patch.object(sys, "stderr", shown):
            with self.assertRaisesRegex(ValueError, "the build failed"):
                runner.cargo_artifacts(["cargo"])
        self.assertIn("error[E0425]: missing", shown.getvalue())

    def test_inherited_git_selection_refuses_before_building(self):
        """Claim: the runner started inside a Git operation refuses and names the variable;
        rejects a run whose fixtures would write the repository GIT_DIR or GIT_WORK_TREE names."""
        for name in ["GIT_DIR", "GIT_WORK_TREE"]:
            with patch.object(runner.subprocess, "run") as run, patch.object(sys, "argv", ["runner", "invariants"]), \
                    patch.dict(runner.os.environ, {name: "/elsewhere"}):
                with self.assertRaisesRegex(ValueError, f"^{name} is set"):
                    runner.main()
            run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
