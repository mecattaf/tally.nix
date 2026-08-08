#!/usr/bin/env python3
"""Cwd-keyed resumable process used by black-box recovery probes."""

from __future__ import annotations

import json
import os
from pathlib import Path
import sys
import time


log = Path(os.environ["FINAL_BAR_RECOVERY_LOG"])
with log.open("a", encoding="utf-8") as handle:
    handle.write(json.dumps({"argv": sys.argv, "cwd": os.getcwd()}) + "\n")
    handle.flush()

print(json.dumps({"thread_id": "final-bar-cwd-session"}), flush=True)
if "hold" in sys.argv:
    time.sleep(4)
