#!/usr/bin/env python3
"""Run tests against an optimized CLI with debug assertions and overflow checks.

The test harness and unit tests keep Cargo's ordinary test profile. Cargo's
artifact receipt identifies the CLI built from this checkout.
"""

import json
import os
from pathlib import Path
import subprocess
import sys


def main():
    build = subprocess.run(
        [
            "cargo", "build", "--locked", "--profile", "cli-test", "--bin", "arc",
            "--message-format=json",
        ],
        stdout=subprocess.PIPE,
        text=True,
    )
    binaries = set()
    for line in build.stdout.splitlines():
        receipt = json.loads(line)
        if receipt.get("reason") == "compiler-message":
            rendered = receipt.get("message", {}).get("rendered")
            if rendered:
                print(rendered, file=sys.stderr, end="")
        if (
            receipt.get("reason") == "compiler-artifact"
            and receipt["target"]["name"] == "arc"
            and receipt.get("executable")
        ):
            binaries.add(str(Path(receipt["executable"]).resolve()))
    if build.returncode:
        return build.returncode
    if len(binaries) != 1:
        print("expected one Cargo artifact for the arc CLI", file=sys.stderr)
        return 1
    env = os.environ.copy()
    env["ARC_TEST_BIN"] = binaries.pop()
    command = sys.argv[1:] or ["cargo", "test", "--locked"]
    os.execvpe(command[0], command, env)


if __name__ == "__main__":
    sys.exit(main())
