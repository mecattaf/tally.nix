#!/usr/bin/env python3
"""No-network implementation fixture behind a Codex-shaped policy adapter."""

import json
import os
from pathlib import Path
import subprocess
import sys
import time


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

if task in {"task-1", "task-3"}:
    (control / f"started-{task}").touch()
    peer = "task-3" if task == "task-1" else "task-1"
    for _ in range(250):
        if (control / f"started-{peer}").exists():
            break
        time.sleep(0.02)
    else:
        raise SystemExit(f"frontier peer {peer} was not dispatched in parallel")

hold = control / f"hold-{task}"
if hold.exists():
    (control / f"holding-{task}").touch()
    while hold.exists():
        time.sleep(0.02)

output = worktree / "build"
output.mkdir(exist_ok=True)

if task == "task-1":
    (output / "one.txt").write_text("one\n", encoding="utf-8")
    (output / "checkpoint-red").write_text("pending phase validation\n", encoding="utf-8")
    # The integration test requests one forbidden artifact, then clears that
    # steering condition before a fresh reconcile attempt proves recovery.
    if (control / "inject-forbidden-path").exists() and not (
        control / "constraint-cleared"
    ).exists():
        (output / "transient.db").write_text("not a database\n", encoding="utf-8")
elif task == "task-2":
    if (output / "one.txt").read_text(encoding="utf-8") != "one\n":
        raise SystemExit("task 2 did not start from task 1's merged result")
    if not (output / "task-2-base.txt").exists():
        base = subprocess.run(
            ["git", "rev-parse", "HEAD"], check=True, text=True, stdout=subprocess.PIPE
        ).stdout.strip()
        (output / "task-2-base.txt").write_text(base + "\n", encoding="utf-8")
    (output / "two.txt").write_text("two\n", encoding="utf-8")
elif task == "task-3":
    (output / "three.txt").write_text("three\n", encoding="utf-8")
elif task == "task-4":
    (output / "four.txt").write_text("four\n", encoding="utf-8")
    (output / "checkpoint-red").unlink()
elif task == "task-5":
    (output / "five.txt").write_text("five\n", encoding="utf-8")
else:
    raise SystemExit(f"unexpected fixture task: {task}")

subprocess.run(["git", "add", "--all", "build"], check=True)
status = subprocess.run(
    ["git", "status", "--porcelain"], check=True, text=True, stdout=subprocess.PIPE
).stdout
if status:
    subprocess.run(["git", "commit", "-m", f"fixture: implement {task}"], check=True)
with (control / "agent-order.log").open("a", encoding="utf-8") as stream:
    stream.write(task + "\n")
