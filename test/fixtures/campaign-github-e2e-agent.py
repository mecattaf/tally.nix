#!/usr/bin/env python3
"""Deterministic implementation worker for the real GitHub campaign e2e."""

import json
import os
from pathlib import Path
import re
import subprocess


brief_path = Path(os.environ["TALLY_BRIEF"])
if not os.environ.get("TALLY_BRIEF_HASH", "").startswith("sha256:"):
    raise SystemExit("campaign agent did not receive its content-addressed brief identity")
brief = json.loads(brief_path.read_text(encoding="utf-8"))
task = brief["task"]
task_id = task["id"]
if task.get("kind") != "implementation":
    raise SystemExit(f"unexpected campaign task kind: {task.get('kind')!r}")
if not re.fullmatch(r"sha256:[0-9a-f]{64}", task.get("revision", "")):
    raise SystemExit("campaign task omitted its admitted source revision")

worktree = Path.cwd()
if task_id == "first":
    output = worktree / "one.txt"
    content = "one\n"
    expected_brief = "Create exactly one.txt"
elif task_id == "second":
    if (worktree / "one.txt").read_text(encoding="utf-8") != "one\n":
        raise SystemExit("dependent task did not start from the first merged result")
    output = worktree / "two.txt"
    content = "two\n"
    expected_brief = "Create exactly two.txt"
else:
    raise SystemExit(f"unexpected campaign e2e task: {task_id!r}")

if expected_brief not in task["brief"]["body"]:
    raise SystemExit(f"task {task_id!r} did not carry its admitted sub-issue brief")
output.write_text(content, encoding="utf-8")
subprocess.run(["git", "add", "--", output.name], check=True)
subprocess.run(["git", "commit", "-m", f"e2e: implement {task_id}"], check=True)
