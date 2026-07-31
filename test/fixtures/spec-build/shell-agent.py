#!/usr/bin/env python3
"""No-network implementation fixture invoked through tally's shell adapter."""

import json
import os
from pathlib import Path
import subprocess
import sys


brief = json.loads(Path(os.environ["TALLY_BRIEF"]).read_text(encoding="utf-8"))
task = brief["task"]["id"]
control = Path(sys.argv[1])
worktree = Path.cwd()
output = worktree / "build"
output.mkdir(exist_ok=True)

if task == "task-1":
    (output / "one.txt").write_text("one\n", encoding="utf-8")
elif task == "task-2":
    if (output / "one.txt").read_text(encoding="utf-8") != "one\n":
        raise SystemExit("task 2 did not start from task 1's merged result")
    base = subprocess.run(
        ["git", "rev-parse", "HEAD"], check=True, text=True, stdout=subprocess.PIPE
    ).stdout.strip()
    (output / "task-2-base.txt").write_text(base + "\n", encoding="utf-8")
    (output / "two.txt").write_text("two\n", encoding="utf-8")
else:
    raise SystemExit(f"unexpected fixture task: {task}")

subprocess.run(["git", "add", "build"], check=True)
subprocess.run(["git", "commit", "-m", f"fixture: implement {task}"], check=True)
with (control / "agent-order.log").open("a", encoding="utf-8") as stream:
    stream.write(task + "\n")
