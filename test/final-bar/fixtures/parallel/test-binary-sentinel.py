#!/usr/bin/env python3
"""Record how flake-probe invokes a stand-in Rust test binary."""

from __future__ import annotations

import json
import os
from pathlib import Path
import sys
import time


record = {
    "argv": sys.argv[1:],
    "rustTestThreads": os.environ.get("RUST_TEST_THREADS"),
}
with Path(os.environ["FINAL_BAR_TEST_BINARY_LOG"]).open("a", encoding="utf-8") as handle:
    handle.write(json.dumps(record, separators=(",", ":")) + "\n")
time.sleep(0.02)
