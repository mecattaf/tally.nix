#!/usr/bin/env python3
"""State-backed GitHub CLI double for hermetic campaign boundary tests."""

from __future__ import annotations

import json
import os
from pathlib import Path
import sys


STATE = Path(os.environ["FINAL_BAR_FORGE_STATE"])
state = json.loads(STATE.read_text(encoding="utf-8"))
arguments = sys.argv[1:]
state.setdefault("calls", []).append(arguments)


def save() -> None:
    STATE.write_text(json.dumps(state, sort_keys=True), encoding="utf-8")


def emit(value: object) -> None:
    print(json.dumps(value, separators=(",", ":")))


def number_from_endpoint(endpoint: str) -> int:
    return int(endpoint.split("?")[0].rstrip("/").split("/")[-1])


def issue(number: int) -> dict[str, object]:
    if number == int(state["master"]["number"]):
        return state["master"]
    for candidate in state.get("subissues", []):
        if int(candidate["number"]) == number:
            return candidate
    raise SystemExit(f"fake-gh: unknown issue {number}")


if arguments[:2] == ["api", "user"]:
    emit({"login": state.get("actor", "operator")})
elif arguments[:2] == ["api", "graphql"]:
    nodes = []
    for candidate in state.get("subissues", []):
        number = str(candidate["number"])
        nodes.append(
            {
                "number": candidate["number"],
                "closedByPullRequestsReferences": {
                    "nodes": state.get("closedByPullRequests", {}).get(number, [])
                },
                "comments": {
                    "pageInfo": {"hasPreviousPage": False},
                    "nodes": state.get("threadComments", {}).get(number, []),
                },
            }
        )
    emit(
        {
            "data": {
                "repository": {
                    "issue": {
                        "subIssues": {
                            "pageInfo": {"hasNextPage": False, "endCursor": None},
                            "nodes": nodes,
                        }
                    }
                }
            }
        }
    )
elif arguments and arguments[0] == "api":
    method = "GET"
    if "--method" in arguments:
        method = arguments[arguments.index("--method") + 1]
    endpoints = [item for item in arguments[1:] if item.startswith("repos/")]
    endpoint = endpoints[0] if endpoints else ""
    if endpoint.endswith("/sub_issues?per_page=100"):
        emit(state.get("subissues", []))
    elif endpoint.endswith("/comments?per_page=100") and "--slurp" in arguments:
        emit([state.get("masterComments", [])])
    elif "/issues/" in endpoint and method == "GET":
        emit(issue(number_from_endpoint(endpoint)))
    elif "/issues/" in endpoint and method in {"PATCH", "POST"}:
        target = issue(number_from_endpoint(endpoint))
        fields = {}
        for index, value in enumerate(arguments):
            if value in {"-f", "-F"} and index + 1 < len(arguments):
                key, _, item = arguments[index + 1].partition("=")
                fields[key] = item
        if "state" in fields:
            target["state"] = fields["state"].lower()
        if "body" in fields:
            target["body"] = fields["body"]
        save()
        emit(target)
    else:
        raise SystemExit(f"fake-gh: unsupported api call: {arguments!r}")
elif arguments[:2] == ["issue", "view"]:
    target = issue(int(arguments[2].rstrip("/").split("/")[-1]))
    emit(target)
elif arguments[:2] in (["issue", "close"], ["issue", "reopen"]):
    target = issue(int(arguments[2].rstrip("/").split("/")[-1]))
    target["state"] = "closed" if arguments[1] == "close" else "open"
    save()
elif arguments[:2] == ["issue", "edit"]:
    target = issue(int(arguments[2].rstrip("/").split("/")[-1]))
    if "--body-file" in arguments:
        target["body"] = Path(arguments[arguments.index("--body-file") + 1]).read_text(
            encoding="utf-8"
        )
    save()
elif arguments[:2] == ["issue", "comment"]:
    target_number = int(arguments[2].rstrip("/").split("/")[-1])
    body = ""
    if "--body-file" in arguments:
        body = Path(arguments[arguments.index("--body-file") + 1]).read_text(encoding="utf-8")
    elif "--body" in arguments:
        body = arguments[arguments.index("--body") + 1]
    ref = f"https://github.com/acme/spec/issues/{target_number}#issuecomment-{len(state.setdefault('postedComments', [])) + 1}"
    state["postedComments"].append({"issue": target_number, "body": body, "url": ref})
    save()
    print(ref)
elif arguments[:2] == ["pr", "list"]:
    emit([])
else:
    raise SystemExit(f"fake-gh: unsupported invocation: {arguments!r}")

save()
