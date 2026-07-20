#!/usr/bin/env python3
# tally — kitty watcher payload (IMPLEMENTATION-PLAN M1.6 `hooks/kitty/tally-watcher.py`; DECISIONS Q4).
#
# This is a kitty WATCHER: a stdlib-only Python module kitty loads and calls on window lifecycle
# edges. It is the EVENT EDGE that replaces the daemon polling `kitty @ ls` for existence — when a
# kitty window is created/closed/focus-changes/title-changes/etc, kitty runs the matching function
# here, which connects to the tally daemon's Unix socket and posts the internal-additive RPC
# `kitty.watcher_event` (CLI-SURFACE §3.1; IMPLEMENTATION-PLAN §3 sensor edges).
#
# REGISTRATION: the `watcher tally-watcher.py` (and/or `window_watcher`) line lives in the
# DOTFILES-owned kitty.conf, NOT here (DECISIONS Q4 boundary). The tally nix module exports THIS
# script's store path read-only (`watcherScript` option, M3.3) so the dotfiles line never rots. This
# file is the payload; it does not install itself.
#
# HARD RULES (mirror the harness-hook discipline, CLI-SURFACE §3.1 / §5 flag 2):
#   * stdlib ONLY — no third-party imports (kitty runs it in its own embedded interpreter).
#   * NEVER block kitty: connect with a short timeout, fire-and-forget the post, swallow every error.
#     A watcher that raises or hangs would stall the terminal — tally must be invisible when the
#     daemon is down. Socket absent / refused / slow ⇒ silently return.
#   * READ-ONLY on kitty state: it inspects the Window object and posts facts; it never launches,
#     creates, or mutates windows. It is a sensor, not an actuator.
#
# The daemon side is `src/kitty/watcher-ingest.ts` (`validateWatcherEvent` / `WatcherIngest`), which
# owns the matching `kitty.watcher_event` param contract.

from __future__ import annotations

import json
import os
import socket
import time
from typing import Any, Dict, Optional


# --- socket path resolution (mirrors src/contracts/paths.ts socketPath) -----------------------

def _runtime_dir() -> str:
    base = os.environ.get("XDG_RUNTIME_DIR")
    if base:
        return base
    # Match paths.ts fallback: /run/user/<uid>.
    try:
        uid = os.getuid()  # type: ignore[attr-defined]
    except AttributeError:
        uid = 1000
    return "/run/user/%d" % uid


def _socket_path() -> str:
    # Allow an explicit override (tests / non-standard layouts), else the XDG runtime path.
    override = os.environ.get("TALLY_SOCKET")
    if override:
        return override
    return os.path.join(_runtime_dir(), "tally", "tally.sock")


# --- the fire-and-forget NDJSON post ----------------------------------------------------------

# A monotonically-increasing request id per process, so each post has a distinct `id` on the wire.
_next_id = 0


def _post_event(payload: Dict[str, Any], timeout: float = 0.25) -> None:
    """Connect to the tally socket and post ONE `kitty.watcher_event` NDJSON request, then return.

    Every failure mode (no socket, connection refused, timeout, broken pipe) is swallowed: tally must
    never block or crash the terminal. We do a best-effort single-line write and do not wait to read
    the daemon's response (the watcher does not need the ack; ingestion is one-way).
    """
    global _next_id
    _next_id += 1
    frame = {
        "id": "watcher-%d-%d" % (os.getpid(), _next_id),
        "method": "kitty.watcher_event",
        "params": payload,
    }
    line = (json.dumps(frame, separators=(",", ":")) + "\n").encode("utf-8")
    path = _socket_path()
    sock: Optional[socket.socket] = None
    try:
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.settimeout(timeout)
        sock.connect(path)
        sock.sendall(line)
    except (OSError, socket.timeout):
        # Daemon down / socket absent / slow — silently give up. tally is invisible when off.
        return
    finally:
        if sock is not None:
            try:
                sock.close()
            except OSError:
                pass


def _iso_now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


# --- window fact extraction (defensive; the Window object's surface varies across kitty versions) --

def _window_id(window: Any) -> Optional[int]:
    wid = getattr(window, "id", None)
    if isinstance(wid, int):
        return wid
    return None


def _window_cwd(window: Any) -> Optional[str]:
    # kitty exposes the foreground process cwd via a couple of accessors across versions.
    for attr in ("cwd_of_child", "current_directory"):
        val = getattr(window, attr, None)
        if callable(val):
            try:
                val = val()
            except Exception:
                val = None
        if isinstance(val, str) and val:
            return val
    return None


def _window_title(window: Any) -> Optional[str]:
    title = getattr(window, "title", None)
    if isinstance(title, str):
        return title
    return None


def _window_is_focused(window: Any) -> Optional[bool]:
    val = getattr(window, "is_focused", None)
    if isinstance(val, bool):
        return val
    return None


def _base_payload(kind: str, window: Any) -> Optional[Dict[str, Any]]:
    """Build the common `kitty.watcher_event` payload for a window edge, or None if unidentifiable."""
    wid = _window_id(window)
    if wid is None:
        return None
    payload: Dict[str, Any] = {"kind": kind, "kitty_window_id": wid, "ts": _iso_now()}
    cwd = _window_cwd(window)
    if cwd is not None:
        payload["cwd"] = cwd
    return payload


# --- kitty watcher callbacks ------------------------------------------------------------------
#
# kitty calls these with (boss, window, data). Each maps a kitty edge onto a `kitty.watcher_event`
# `kind` the daemon's WatcherIngest understands. All are wrapped so an exception never escapes into
# kitty.


def on_load(boss: Any, data: Any) -> None:
    # Module load — nothing to post. Present so kitty's watcher loader is happy.
    return None


def on_focus_change(boss: Any, window: Any, data: Any) -> None:
    try:
        payload = _base_payload("focus_change", window)
        if payload is None:
            return
        focused = None
        if isinstance(data, dict):
            focused = data.get("focused")
        if not isinstance(focused, bool):
            focused = _window_is_focused(window)
        if isinstance(focused, bool):
            payload["is_focused"] = focused
        _post_event(payload)
    except Exception:
        return None


def on_close(boss: Any, window: Any, data: Any) -> None:
    try:
        payload = _base_payload("window_closed", window)
        if payload is None:
            return
        _post_event(payload)
    except Exception:
        return None


def on_cmd_startstop(boss: Any, window: Any, data: Any) -> None:
    try:
        # data carries {"is_start": bool, ...} across kitty versions.
        is_start = None
        if isinstance(data, dict):
            is_start = data.get("is_start")
        kind = "cmd_start" if is_start else "cmd_stop"
        payload = _base_payload(kind, window)
        if payload is None:
            return
        _post_event(payload)
    except Exception:
        return None


def on_title_change(boss: Any, window: Any, data: Any) -> None:
    try:
        payload = _base_payload("title_change", window)
        if payload is None:
            return
        title = None
        if isinstance(data, dict):
            title = data.get("title")
        if not isinstance(title, str):
            title = _window_title(window)
        if isinstance(title, str):
            payload["title"] = title
        _post_event(payload)
    except Exception:
        return None


def on_set_user_var(boss: Any, window: Any, data: Any) -> None:
    try:
        payload = _base_payload("user_var_change", window)
        if payload is None:
            return
        if isinstance(data, dict):
            key = data.get("key")
            value = data.get("value")
            if isinstance(key, str):
                payload["user_var_key"] = key
            if isinstance(value, str):
                payload["user_var_value"] = value
        _post_event(payload)
    except Exception:
        return None
