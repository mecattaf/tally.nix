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
role = brief.get("role", "implementation")
control = Path(sys.argv[1])
worktree = Path.cwd()
launch = sys.argv[2:]
expected_sandbox = "read-only" if role == "diagnosis" else "danger-full-access"
expected_prefix = [
    "-c",
    'approval_policy="never"',
    "--sandbox",
    expected_sandbox,
    # The campaign's configured model, rendered through the adapter's own
    # authorized override. It is what the daemon records as this job's
    # canonical model and therefore what the merge node may name.
    "--model",
    "fixture/policy-agent-1",
    "--",
]
if launch[:7] != expected_prefix or len(launch) != 8 or "TALLY_BRIEF" not in launch[7]:
    detail = f"adapter policy launch was not preserved: {launch!r}\n"
    (control / "policy-error.log").write_text(detail, encoding="utf-8")
    raise SystemExit(detail)

with (control / "policy-order.log").open("a", encoding="utf-8") as stream:
    stream.write(f"{role}:{task}:{launch[1]}:{launch[3]}\n")

if role == "diagnosis":
    required = {"failure", "gateOutputs", "taskBrief", "diff", "previousDiagnoses"}
    missing = sorted(required - set(brief))
    if missing:
        raise SystemExit(f"diagnosis brief omitted inputs: {missing!r}")
    failure = brief["failure"]
    diff = brief["diff"]
    gates = brief["gateOutputs"]
    task_brief = brief["taskBrief"]
    if task_brief["task"]["id"] != task:
        raise SystemExit("diagnosis did not receive the failed task brief")
    expected = {
        "task-1": "build/one.txt",
        "task-2": "build/two.txt",
    }.get(task)
    if not diff["available"]:
        raise SystemExit("diagnosis diff input was unavailable")
    if expected is not None and expected not in diff["patch"]:
        raise SystemExit(f"diagnosis diff omitted {expected}")
    if not gates:
        raise SystemExit("diagnosis did not receive gate outputs")
    if task == "task-1":
        if "build/transient.db" not in failure["captureStderr"]:
            raise SystemExit("task 1 diagnosis omitted constraint stderr")
        # Names a task id containing "sk-", a git object id, and prose about a
        # token bug. Redaction must keep all three and drop only the credential.
        steering = (
            "task-1 left build/transient.db behind; the subtask-2 cleanup never ran.\n"
            "Rebase onto 6347cbb9f4a2b1c0d5e6f70819a2b3c4d5e6f708 and rerun the gates.\n"
            "The auth token bug is unrelated to this failure.\n"
            "Do not expose ghp_0123456789abcdefghijklmnopqrstuvwxyz in public output."
        )
    elif task == "task-2":
        if "task 2 deterministic gate failure" not in failure["captureStderr"]:
            raise SystemExit("task 2 diagnosis omitted command-gate stderr")
        steering = "The deterministic task 2 gate remains red; inspect build/two.txt and retry."
    elif task == "phase-one-checkpoint":
        if "phase one checkpoint has no prior steering" not in failure["captureStderr"]:
            raise SystemExit("checkpoint diagnosis omitted checkpoint stderr")
        steering = (
            "The phase-one checkpoint reached its own attempt without durable steering. "
            "Retry now that the accumulated tree is settled."
        )
    else:
        raise SystemExit(f"unexpected diagnosis task: {task}")
    receipt = {
        "task": task,
        "stage": failure["stage"],
        "gateCount": len(gates),
        "previous": len(brief["previousDiagnoses"]),
        "hasBrief": task_brief["task"]["id"] == task,
        "hasDiff": diff["available"],
        "hasPatch": bool(diff["patch"]),
        "hasStderr": bool(failure["captureStderr"]),
    }
    with (control / "diagnosis-inputs.log").open("a", encoding="utf-8") as stream:
        stream.write(json.dumps(receipt, sort_keys=True) + "\n")
    print("TALLY_FINAL_MESSAGE=" + json.dumps(steering))
    raise SystemExit(0)

if role != "implementation":
    raise SystemExit(f"unexpected fixture role: {role}")

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
    steering = brief["steering"]["machineDiagnoses"]
    if not steering:
        (output / "transient.db").write_text("not a database\n", encoding="utf-8")
    else:
        diagnosis = steering[0]["diagnosis"]
        if len(steering) != 1 or "[redacted-token]" not in diagnosis:
            raise SystemExit("task 1 retry did not receive redacted machine steering")
        for survivor in (
            "task-1",
            "subtask-2",
            "6347cbb9f4a2b1c0d5e6f70819a2b3c4d5e6f708",
            "The auth token bug is unrelated",
        ):
            if survivor not in diagnosis:
                raise SystemExit(f"redaction destroyed steering content: {survivor!r}")
        (control / "task-1-steering-visible").touch()
elif task in {"task-2", "task-2b"}:
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
elif task == "task-6":
    (output / "six.txt").write_text("six\n", encoding="utf-8")
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
