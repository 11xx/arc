#!/usr/bin/env python3
"""Run exact invariant witnesses or an explicitly requested area suite.

Cargo artifact receipts bind harnesses and the spawned CLI to this checkout,
and the selection handed to the harnesses carries a receipt naming that build.
The checked-in list is the complete integration selection.
"""

import argparse
import collections
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
Artifact = collections.namedtuple("Artifact", "path debug_assertions")


def refuse_inside_git_operation(environ):
    """Git exports these to commands it runs, and a fixture Git started under
    one writes the repository it names."""
    for name in ("GIT_DIR", "GIT_WORK_TREE"):
        if name in environ:
            raise ValueError(f"{name} is set: the tests would reach the repository it names. "
                             "Run them outside any Git operation, with GIT_DIR and GIT_WORK_TREE unset.")


def clear_git_environment(environ):
    """Every inherited GIT_ variable leaves before Cargo starts: a build script
    running Git, or a harness, would otherwise be steered by a trace target,
    template or selector the caller's environment named."""
    for name in [name for name in environ if name.startswith("GIT_")]:
        del environ[name]


def cargo_artifacts(command):
    build = subprocess.run(command, stdout=subprocess.PIPE, text=True)
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
        found[key] = Artifact(str(Path(receipt["executable"]).resolve()),
                              receipt["profile"]["debug_assertions"])
    if build.returncode != 0:
        raise ValueError(f"cargo exited {build.returncode}; the build failed")
    return found


def artifacts(profile=PROFILE):
    found = cargo_artifacts(["cargo", "test", "--locked", "--profile", profile, "--no-run",
                             "--message-format=json"])
    if not {"arc", "unit", "cli"} <= found.keys():
        raise ValueError("Cargo did not identify the CLI and both test harnesses")
    return found


def identity(path):
    status = os.stat(path)
    return f"{status.st_dev}:{status.st_ino}:{status.st_mtime_ns}:{status.st_size}"


def selection(executable, cargo_arc):
    """The environment selecting `executable` for the fixtures of the harnesses
    built beside `cargo_arc`. The receipt names that build by the identity of
    Cargo's own arc executable and the executable by its resolved path and
    identity, which the harness compares with what it finds, so a receipt
    vouches for one file of one build."""
    resolved = str(Path(executable.path).resolve())
    assertions = "true" if executable.debug_assertions else "false"
    return {"ARC_TEST_BIN": executable.path,
            "ARC_TEST_BIN_RECEIPT": f"build={identity(cargo_arc.path)} "
                                    f"executable={identity(resolved)}:{resolved} "
                                    f"debug-assertions={assertions}"}


def harness_environment(environ, selected):
    """What the harnesses run under: the caller's environment without any
    child-mode switch, which only a parent harness may set, the selection, and
    a demand that the harness confine its writes or refuse to run."""
    env = {name: value for name, value in environ.items() if not name.startswith("ARC_TEST_CHILD_")}
    env.update(selected)
    env["ARC_TEST_CONFINEMENT"] = "required"
    return env


def inventory(binary, env):
    output = subprocess.check_output([binary, "--list", "--format=terse"], env=env, text=True)
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
    inventories = {target: inventory(binaries[target].path, env) for target in {"unit", "cli"}}
    validate(rows, inventories)
    cpus = len(os.sched_getaffinity(0)) if hasattr(os, "sched_getaffinity") else (os.cpu_count() or 1)
    workers = int(env.get("RUST_TEST_THREADS", min(64, cpus * 4)))
    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
        results = list(pool.map(lambda row: run_one(binaries[row["target"]].path, row["test"], env), rows))
    timings = ["arc-check-timings/1"]
    for index, (row, (name, ms, passed, output)) in enumerate(zip(rows, results), 1):
        label = row["invariant"] + ":" + row["target"] + ":" + name
        if len(label.encode()) > 128:
            label = row["invariant"] + ":" + str(index)
        timings.append(f'{label}\t{ms}\t{"pass" if passed else "fail"}')
        print(f'{row["invariant"]} {name}: {"pass" if passed else "FAIL"} ({ms}ms)')
        if not passed:
            print(output, file=sys.stderr)
    report = ROOT / "target/invariant-timings.tsv"
    report.parent.mkdir(exist_ok=True)
    report.write_text("\n".join(timings) + "\n")
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
    refuse_inside_git_operation(os.environ)
    clear_git_environment(os.environ)
    os.chdir(ROOT)
    binaries = artifacts()
    env = harness_environment(os.environ, selection(binaries["arc"], binaries["arc"]))
    if args.mode == "invariants":
        with (ROOT / ".arc/invariants.tsv").open() as file:
            return run_invariants(witnesses(file), binaries, env)
    if args.mode == "release":
        release = cargo_artifacts(["cargo", "build", "--locked", "--release", "--bin", "arc",
                                   "--message-format=json"])
        env.update(selection(release["arc"], binaries["arc"]))
        args.target, args.module = "cli", "release"
    harness = binaries[args.target].path
    names = sorted(name for name in inventory(harness, env) if name.rpartition("::")[0] == args.module)
    if not names:
        raise ValueError(f"no tests in exact module {args.module}")
    return subprocess.call([harness, "--exact", *names], env=env)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, subprocess.CalledProcessError) as error:
        print(error, file=sys.stderr)
        sys.exit(1)
