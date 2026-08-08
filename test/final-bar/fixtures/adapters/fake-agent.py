#!/usr/bin/env python3
"""Deterministic Codex/Pi shaped process with an argv spawn log."""

from __future__ import annotations

import json
import os
from pathlib import Path
import sys


log = Path(os.environ["FINAL_BAR_ADAPTER_LOG"])
with log.open("a", encoding="utf-8") as handle:
    handle.write(json.dumps(sys.argv) + "\n")

shape = os.environ.get("FINAL_BAR_ADAPTER_SHAPE", "codex")
if shape == "pi":
    print(json.dumps({"type": "session", "id": "pi-final-bar", "cwd": os.getcwd()}))
    print(
        json.dumps(
            {
                "type": "message_end",
                "message": {
                    "role": "assistant",
                    "model": "pi-fixture-model",
                    "stopReason": "stop",
                    "content": [{"type": "text", "text": "done"}],
                    "usage": {
                        "input": 1,
                        "output": 1,
                        "cacheRead": 0,
                        "cacheWrite": 0,
                        "reasoning": 0,
                        "totalTokens": 2,
                        "cost": {
                            "input": 0,
                            "output": 0,
                            "cacheRead": 0,
                            "cacheWrite": 0,
                            "total": 0
                        }
                    }
                }
            }
        )
    )
else:
    print(json.dumps({"type": "thread.started", "thread_id": "codex-final-bar-thread"}))
    print(
        json.dumps(
            {
                "type": "item.completed",
                "item": {"id": "answer", "type": "agent_message", "text": "done"}
            }
        )
    )
    print(
        json.dumps(
            {
                "type": "turn.completed",
                "usage": {
                    "input_tokens": 2,
                    "cached_input_tokens": 1,
                    "cache_write_input_tokens": 0,
                    "output_tokens": 1,
                    "reasoning_output_tokens": 0
                }
            }
        )
    )
