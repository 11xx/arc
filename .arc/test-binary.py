#!/usr/bin/env python3
"""Run exact invariant witnesses or an explicitly requested area suite.

Cargo artifact receipts bind harnesses and the spawned CLI to this checkout.
The checked-in list is the complete integration selection.
"""

import argparse
import concurrent.futures
import csv
import json
import os
from pathlib import Path
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parent.parent
PROFILE = "dev"


def cargo_artifacts(command):
    build = subprocess.run(command, stdout=subprocess.PIPE, text=True, check=True)
    found = {}
    for line in build.stdout.splitlines():
        receipt = json.loads(line)
        if receipt.get("reason") == "compiler-message":
            print(receipt["message"].get("rendered", ""), file=sys.stderr, end="")
        if receipt.get("reason") != "compiler-artifact" or not receipt.get("executable"):
            continue
        target = receipt["target"]
        key = target["name"]
        if key == "arc" and receipt["profile"]["test"]:
            key = "unit"
        if key in found:
            raise ValueError(f"ambiguous Cargo artifact: {key}")
        found[key] = str(Path(receipt["executable"]).resolve())
    return found


def artifacts(profile=PROFILE):
    found = cargo_artifacts(["cargo", "test", "--locked", "--profile", profile, "--no-run",
                             "--message-format=json"])
    if not {"arc", "unit", "cli"} <= found.keys():
        raise ValueError("Cargo did not identify the CLI and both test harnesses")
    return found


def inventory(binary):
    output = subprocess.check_output([binary, "--list", "--format=terse"], text=True)
    return {line.removesuffix(": test") for line in output.splitlines() if line.endswith(": test")}


def witnesses(file):
    rows = list(csv.DictReader(file, delimiter="\t"))
    seen = set()
    for row in rows:
        key = row["target"], row["test"]
        if (row["target"] not in {"unit", "cli"} or key in seen
                or not row["claim"] or not row["invariant"]):
            raise ValueError(f"invalid or duplicate witness: {key}")
        seen.add(key)
    if not rows:
        raise ValueError("the invariant list is empty")
    return rows


def validate(rows, inventories):
    missing = [f'{r["target"]}:{r["test"]}' for r in rows
               if r["test"] not in inventories[r["target"]]]
    if missing:
        raise ValueError("missing exact witnesses: " + ", ".join(missing))


def run_one(binary, name, env):
    start = time.monotonic_ns()
    result = subprocess.run([binary, "--exact", name, "--nocapture"], env=env,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    passed = result.returncode == 0 and "test result: ok. 1 passed; 0 failed; 0 ignored;" in result.stdout
    return name, (time.monotonic_ns() - start) // 1_000_000, passed, result.stdout


def run_invariants(rows, binaries, env):
    inventories = {target: inventory(binaries[target]) for target in {"unit", "cli"}}
    validate(rows, inventories)
    cpus = len(os.sched_getaffinity(0)) if hasattr(os, "sched_getaffinity") else (os.cpu_count() or 1)
    workers = int(env.get("RUST_TEST_THREADS", min(64, cpus * 4)))
    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
        results = list(pool.map(lambda row: run_one(binaries[row["target"]], row["test"], env), rows))
    timings = ["arc-check-timings/1"]
    for index, (row, (name, ms, passed, output)) in enumerate(zip(rows, results), 1):
        label = row["invariant"] + ":" + row["target"] + ":" + name
        if len(label.encode()) > 128:
            label = row["invariant"] + ":" + str(index)
        timings.append(f'{label}\t{ms}\t{"pass" if passed else "fail"}')
        print(f'{row["invariant"]} {name}: {"pass" if passed else "FAIL"} ({ms}ms)')
        if not passed:
            print(output, file=sys.stderr)
    (ROOT / ".arc/invariant-timings.tsv").write_text("\n".join(timings) + "\n")
    return 0 if all(r[2] for r in results) else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="mode", required=True)
    sub.add_parser("invariants")
    area = sub.add_parser("area")
    area.add_argument("target", choices=["unit", "cli"])
    area.add_argument("module", help="exact module path, for example gate_raises or store::tests")
    sub.add_parser("release")
    args = parser.parse_args()
    os.chdir(ROOT)
    binaries = artifacts()
    env = os.environ.copy()
    env["ARC_TEST_BIN"] = binaries["arc"]
    if args.mode == "invariants":
        with (ROOT / ".arc/invariants.tsv").open() as file:
            return run_invariants(witnesses(file), binaries, env)
    if args.mode == "release":
        release = cargo_artifacts(["cargo", "build", "--locked", "--release", "--bin", "arc",
                                   "--message-format=json"])
        env["ARC_TEST_BIN"] = release["arc"]
        args.target, args.module = "cli", "release"
    names = sorted(name for name in inventory(binaries[args.target]) if name.rpartition("::")[0] == args.module)
    if not names:
        raise ValueError(f"no tests in exact module {args.module}")
    return subprocess.call([binaries[args.target], "--exact", *names], env=env)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, subprocess.CalledProcessError) as error:
        print(error, file=sys.stderr)
        sys.exit(1)
