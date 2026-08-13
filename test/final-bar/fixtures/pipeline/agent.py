#!/usr/bin/env python3
"""Deterministic one-task campaign agent for the hermetic full pipeline."""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess


brief_path = Path(os.environ["TALLY_BRIEF"])
brief = json.loads(brief_path.read_text(encoding="utf-8"))
task = brief["task"]
if task["id"] != "task-one" or task["kind"] != "implementation":
    raise SystemExit(f"unexpected task: {task!r}")
if "Create result.txt." not in task["brief"]["body"]:
    raise SystemExit("sub-issue brief did not cross the campaign boundary")

proof = Path(os.environ["FINAL_BAR_PIPELINE_PROOF"])
proof.write_text(json.dumps({"brief": brief}), encoding="utf-8")

Path("result.txt").write_text("final-bar\n", encoding="utf-8")
subprocess.run(["git", "add", "--", "result.txt"], check=True)
subprocess.run(["git", "commit", "-m", "final-bar: implement task one"], check=True)
