#!/usr/bin/env python3
"""Protocol fixture for the NixOS remote-placement and hardening check."""

import json
import os
import pathlib
import socket
import subprocess
import sys
import threading


def write_json(stream, value):
    stream.sendall(json.dumps(value, separators=(",", ":")).encode() + b"\n")


def trace_server(listener, marker):
    while True:
        connection, _ = listener.accept()
        marker.write_text("trace-connected\n")
        with connection:
            while connection.recv(65536):
                pass


def install_note(worktree, home, attributes):
    revision = (
        subprocess.run(
            ["git", "-C", worktree, "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        )
        .stdout.strip()
        .lower()
    )
    metadata = {
        "schema_version": "authorship/3.0.0",
        "base_commit_sha": revision,
        "git_ai_version": "1.6.17",
        "prompts": {
            "remote-placement": {
                "custom_attributes": attributes,
                "agent_id": {
                    "tool": "remote-fixture",
                    "id": "nixos-remote-session",
                    "model": "nixos-remote-model",
                },
            }
        },
        "sessions": {},
    }
    note = home / "note"
    note.write_text(
        "NixOS remote-placement fixture\n---\n"
        + json.dumps(metadata, separators=(",", ":"))
        + "\n"
    )
    subprocess.run(
        [
            "git",
            "-C",
            worktree,
            "notes",
            "--ref",
            "refs/notes/ai",
            "add",
            "-f",
            "-F",
            str(note),
            revision,
        ],
        check=True,
        capture_output=True,
    )


def serve():
    home = pathlib.Path(os.environ["GIT_AI_DAEMON_HOME"])
    home.mkdir(parents=True, exist_ok=True)
    attributes = json.loads(os.environ["GIT_AI_CUSTOM_ATTRIBUTES"])
    (home / "fixture-observation.json").write_text(
        json.dumps(
            {
                "host": socket.gethostname(),
                "attributes": attributes,
                "home": str(home),
            },
            separators=(",", ":"),
        )
        + "\n"
    )

    control_path = os.environ["GIT_AI_DAEMON_CONTROL_SOCKET"]
    trace_path = os.environ["GIT_AI_DAEMON_TRACE_SOCKET"]
    for path in [control_path, trace_path]:
        try:
            os.unlink(path)
        except FileNotFoundError:
            pass
    trace_listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    trace_listener.bind(trace_path)
    trace_listener.listen()
    threading.Thread(
        target=trace_server,
        args=(trace_listener, home / "trace-observed"),
        daemon=True,
    ).start()

    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    listener.bind(control_path)
    listener.listen()
    running = True
    while running:
        connection, _ = listener.accept()
        with connection:
            request = json.loads(connection.makefile("rb").readline())
            method = request.get("method")
            if method == "ping":
                write_json(connection, {"ok": True, "seq": 0, "data": {}})
            elif method == "sync.family":
                try:
                    install_note(
                        request["params"]["repo_working_dir"],
                        home,
                        attributes,
                    )
                    write_json(connection, {"ok": True, "seq": 1, "data": {}})
                except Exception as error:
                    write_json(connection, {"ok": False, "error": str(error)})
            elif method == "shutdown":
                write_json(connection, {"ok": True, "seq": 2, "data": {}})
                running = False
            else:
                write_json(
                    connection,
                    {"ok": False, "error": f"unsupported method {method!r}"},
                )


if sys.argv[1:] == ["--version"]:
    print("1.6.17")
elif sys.argv[1:] == ["bg", "run"]:
    serve()
else:
    raise SystemExit(f"unsupported fixture invocation: {sys.argv[1:]!r}")
