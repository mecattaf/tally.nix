#!/usr/bin/env python3
"""Exec the packaged driver binary with the fixture PATH."""

from __future__ import annotations

import os
import sys


driver = os.environ["FINAL_BAR_DRIVER"]
os.execv(driver, [driver, *sys.argv[1:]])
