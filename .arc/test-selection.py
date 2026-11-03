#!/usr/bin/env python3
"""Select integration tests from the committed master-to-HEAD change.

Only nonempty Markdown-only deltas skip tests. Uncertainty runs cargo test.
The environment probe binds narrow evidence to master's exact head and tools.
"""

import argparse
import json
from pathlib import Path
import re
import subprocess
import sys

TARGET = "refs/heads/master"
GATE = "test-integration"
FULL = ("full", [], False)


def git(*args):
    return (
        subprocess.check_output(["git", *args], stderr=subprocess.PIPE)
        .decode("utf-8", errors="strict")
        .strip("\n")
    )


def delta():
    try:
        target = git("rev-parse", "--verify", TARGET + "^{commit}")
        base = git("merge-base", target, "HEAD")
        return git(
            "diff", "--no-renames", "--name-only", "-z", base, "HEAD", "--"
        ).split("\0")[:-1]
    except (OSError, UnicodeError, subprocess.CalledProcessError):
        return None


def select(paths):
    if not paths or any(not isinstance(path, str) or not path for path in paths):
        return FULL
    if all(path.endswith(".md") for path in paths):
        return ("markdown", [], False)
    try:
        modules = set(
            re.findall(r"^mod (\w+);$", Path("tests/cli/main.rs").read_text(), re.M)
        )
    except (OSError, UnicodeError):
        return FULL
    selected = set()
    units = False
    for path in paths:
        if path.endswith(".md"):
            continue
        name = Path(path).stem
        if (
            path in ("tests/cli/common.rs", "tests/cli/main.rs")
            or name not in modules
            or not Path(f"tests/cli/{name}.rs").is_file()
        ):
            return FULL
        if path == f"tests/cli/{name}.rs":
            selected.add(name)
        elif path.startswith("src/") and path.endswith(".rs"):
            selected.add(name)
            units = True
        else:
            return FULL
    return ("modules", sorted(selected), units) if selected else FULL


def selector():
    try:
        request = json.load(sys.stdin)
        if (
            request["schema"] != "arc-gate-delta/1"
            or not all(
                isinstance(request[key], str) and request[key]
                for key in ("source_tree", "target_tree")
            )
            or not isinstance(request["paths"], list)
            or not isinstance(request["gates"], list)
            or not all(isinstance(gate, str) for gate in request["gates"])
        ):
            raise ValueError("invalid gate delta")
        affected = request["gates"]
        if select(request["paths"])[0] == "markdown":
            affected = [gate for gate in affected if gate != GATE]
        answer = {"complete": True, "gates": affected}
    except (KeyError, TypeError, ValueError):
        answer = {"complete": False, "gates": []}
    print(json.dumps({"schema": "arc-gate-selection/1", **answer}, sort_keys=True))


def environment():
    try:
        target = git("rev-parse", "--verify", TARGET + "^{commit}")
    except (OSError, UnicodeError, subprocess.CalledProcessError):
        target = "unavailable: full suite"
    tools = [
        subprocess.check_output([tool, "--version"], text=True).strip()
        for tool in ("cargo", "rustc")
    ]
    print(
        json.dumps(
            {"target": target, "tools": tools, "python": sys.version}, sort_keys=True
        )
    )


def run(plan):
    mode, modules, units = plan
    print(
        f"integration tests: {mode}; modules={','.join(modules)}; units={units}",
        flush=True,
    )
    if mode == "markdown":
        return 0
    if mode == "modules":
        listed = subprocess.run(
            ["cargo", "test", "--test", "cli", "--", "--list"],
            capture_output=True,
            text=True,
        )
        if listed.returncode or any(
            not any(
                line.startswith(name + "::") and line.endswith(": test")
                for line in listed.stdout.splitlines()
            )
            for name in modules
        ):
            print(
                "module inventory unavailable or empty: running full suite", flush=True
            )
            mode = "full"
    commands = (
        [[sys.executable, ".arc/test_selection_test.py"], ["cargo", "test"]]
        if mode == "full"
        else (
            ([["cargo", "test", "--bin", "arc"]] if units else [])
            + [
                [
                    "cargo",
                    "test",
                    "--test",
                    "cli",
                    "--",
                    *[name + "::" for name in modules],
                ]
            ]
        )
    )
    for command in commands:
        result = subprocess.run(command)
        if result.returncode:
            return result.returncode
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument(
        "--selector", action="store_true", help="read arc-gate-delta/1 on stdin"
    )
    modes.add_argument(
        "--environment",
        action="store_true",
        help="print the target and toolchain identity",
    )
    modes.add_argument(
        "--plan", action="store_true", help="print selection without running tests"
    )
    args = parser.parse_args()
    if args.selector:
        selector()
    elif args.environment:
        environment()
    elif args.plan:
        print(json.dumps(select(delta())))
    else:
        return run(select(delta()))
    return 0


if __name__ == "__main__":
    sys.exit(main())
