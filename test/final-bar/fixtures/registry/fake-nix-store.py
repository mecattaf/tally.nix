#!/usr/bin/env python3
"""Indirect-root recorder with enough nix-store semantics for ownership tests."""

from __future__ import annotations

import json
import os
from pathlib import Path
import sys


log = Path(os.environ["FINAL_BAR_NIX_STORE_LOG"])
arguments = sys.argv[1:]
with log.open("a", encoding="utf-8") as handle:
    handle.write(json.dumps(arguments) + "\n")

if "--add-root" in arguments and "--realise" in arguments:
    link = Path(arguments[arguments.index("--add-root") + 1])
    target = Path(arguments[arguments.index("--realise") + 1])
    link.parent.mkdir(parents=True, exist_ok=True)
    if link.exists() or link.is_symlink():
        link.unlink()
    link.symlink_to(target)
    print(target)
elif "--realise" in arguments:
    print(arguments[arguments.index("--realise") + 1])
