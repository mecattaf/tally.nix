#!/usr/bin/env python3
"""Exec the immutable payload behind the packaged driver with fixture PATH."""

from __future__ import annotations

import os
import sys


script = os.environ["FINAL_BAR_DRIVER_SCRIPT"]
os.execv(sys.executable, [sys.executable, script, *sys.argv[1:]])
