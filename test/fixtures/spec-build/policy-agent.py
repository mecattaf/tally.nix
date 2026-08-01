#!/usr/bin/env python3
"""No-network implementation fixture behind a Codex-shaped policy adapter."""

import json
import os
from pathlib import Path
import subprocess
import sys


brief = json.loads(Path(os.environ["TALLY_BRIEF"]).read_text(encoding="utf-8"))
task = brief["task"]["id"]
control = Path(sys.argv[1])
worktree = Path.cwd()
launch = sys.argv[2:]
expected_prefix = [
    "--ask-for-approval",
    "on-request",
    "--sandbox",
    "workspace-write",
    "--",
]
if launch[:5] != expected_prefix or len(launch) != 6 or "TALLY_BRIEF" not in launch[5]:
    detail = f"adapter policy launch was not preserved: {launch!r}\n"
    (control / "policy-error.log").write_text(detail, encoding="utf-8")
    raise SystemExit(detail)

with (control / "policy-order.log").open("a", encoding="utf-8") as stream:
    stream.write(f"{task}:{launch[1]}:{launch[3]}\n")

output = worktree / "build"
output.mkdir(exist_ok=True)

if task == "task-1":
    (output / "one.txt").write_text("one\n", encoding="utf-8")
    subprocess.run(["git", "add", "build/one.txt"], check=True)
    subprocess.run(["git", "commit", "-m", f"fixture: implement {task}"], check=True)
    # Keep the artifact in its own commit so the integration test can prove
    # that deleting it in a later commit remains red, then rewrite the branch
    # to the clean implementation commit as the documented remediation.
    (output / "transient.db").write_text("not a database\n", encoding="utf-8")
    subprocess.run(["git", "add", "build/transient.db"], check=True)
    subprocess.run(["git", "commit", "-m", "fixture: add forbidden artifact"], check=True)
elif task == "task-2":
    if (output / "one.txt").read_text(encoding="utf-8") != "one\n":
        raise SystemExit("task 2 did not start from task 1's merged result")
    base = subprocess.run(
        ["git", "rev-parse", "HEAD"], check=True, text=True, stdout=subprocess.PIPE
    ).stdout.strip()
    (output / "task-2-base.txt").write_text(base + "\n", encoding="utf-8")
    (output / "two.txt").write_text("two\n", encoding="utf-8")
    subprocess.run(["git", "add", "build"], check=True)
    subprocess.run(["git", "commit", "-m", f"fixture: implement {task}"], check=True)
else:
    raise SystemExit(f"unexpected fixture task: {task}")
with (control / "agent-order.log").open("a", encoding="utf-8") as stream:
    stream.write(task + "\n")
