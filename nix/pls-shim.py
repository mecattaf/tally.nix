#!/usr/bin/env python3
# tally-owned `pls` client shim (M1.5 packaging seam; DECISIONS Q9; SPEC "pls IS the per-box governor").
#
# WHY THIS EXISTS: tally's broker client (src/pls/broker.ts) binds to a lease-broker CLI surface —
#   pls acquire  --pool <p> --cost <c> --priority <n> [--tenant <t>]
#   pls release  --lease <id>
#   pls status   --pool <p>
#   pls coalloc  --pools p1,p2 --costs c1,c2 --priority <n> [--tenant <t>]
# emitting JSON ({lease_id, generation, granted, ...}). The pinned upstream `pls` binary
# (github:sniarchos/pls) ships only `run|status|watch` over an HTTP broker and knows nothing of
# acquire/release/coalloc/--pool or `generation` — so wrapping it verbatim leaves tally's whole
# lease→dispatch path dead in production. This shim maps tally's CLI onto the upstream broker's HTTP
# API (POST /acquire, POST /release/<id>, GET /status), synthesizing the fields tally needs and
# printing exactly the JSON broker.ts parses. It is dependency-free (stdlib http only).
#
# ADDRESSING (finding: per-pool broker ports): the target broker URL is resolved PER POOL from the
# env the daemon/drain unit sets — PLS_URL takes precedence, else PLS_POOL_URLS (a JSON map of
# pool->url the module renders from basePort+priority), else PLS_PORT/PLS_HOST, else the default
# 127.0.0.1:5555. So controller-gpu traffic reaches the controller broker, not the worker's.
#
# GENERATION (lease_epoch primary source, PS#21): the upstream broker has no generation counter, so
# the shim keeps a per-broker monotonic counter in $XDG_STATE_HOME/tally/pls-generation and bumps it
# on every GRANT — monotone across grants so it fences zombies exactly as tally expects. Every bump
# floors at the daemon's boot-fence counter ($XDG_STATE_HOME/tally/epoch) too, and the daemon's boot
# floors at THIS file in turn (src/daemon/epoch.ts), so grant generations and daemon restart fences
# form ONE totally-ordered lease_epoch series — never two independent counters mixed under one wire
# name (issue #4). Each file keeps a single writer: the shim owns pls-generation, the daemon owns
# epoch; they only READ each other.
#
# LEASE INSPECTION (issue #8): `pls status --pool <p>` above is the FROZEN wire contract broker.ts
# parses (held/queued as counts) and must never change shape. `pls list` (and `pls status` invoked
# with NO --pool) is a separate, shim-only, operator-facing subcommand — not part of tally's CLI
# surface (CLI-SURFACE freezes only the tally verb set) — that iterates every pool this box knows
# about (PLS_POOL_URLS when set, else the single default broker) and prints the actual held/waiting
# TICKETS (id/age/ttl), not just their counts, so an operator can diagnose lease state without raw
# curl against the broker.
#
# Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import json
import os
import sys
import time
import urllib.request
import urllib.error


def _state_dir():
    base = os.environ.get("XDG_STATE_HOME") or os.path.join(os.path.expanduser("~"), ".local", "state")
    d = os.path.join(base, "tally")
    os.makedirs(d, exist_ok=True)
    return d


def _broker_url(pool):
    # PLS_URL (explicit) wins; then a per-pool url map; then PLS_HOST/PLS_PORT; then the default.
    explicit = os.environ.get("PLS_URL")
    if explicit:
        return explicit.rstrip("/")
    pool_urls = os.environ.get("PLS_POOL_URLS")
    if pool_urls:
        try:
            m = json.loads(pool_urls)
            if pool in m and m[pool]:
                return str(m[pool]).rstrip("/")
        except (ValueError, TypeError):
            pass
    host = os.environ.get("PLS_HOST", "127.0.0.1")
    port = os.environ.get("PLS_PORT", "5555")
    return "http://{}:{}".format(host, port)


def _all_pool_urls():
    # Every (pool_name, broker_url) pair this box knows about — for `list`/`status` with no --pool
    # (issue #8). Same precedence as _broker_url: an explicit PLS_URL means there is exactly one
    # broker reachable regardless of pool name (pool name unknown, reported as None); otherwise
    # PLS_POOL_URLS (the per-pool map the daemon/drain unit sets) is enumerated in full; otherwise
    # there is just the single default broker (pool name unknown).
    explicit = os.environ.get("PLS_URL")
    if explicit:
        return [(None, explicit.rstrip("/"))]
    pool_urls = os.environ.get("PLS_POOL_URLS")
    if pool_urls:
        try:
            m = json.loads(pool_urls)
            if m:
                return [(name, str(url).rstrip("/")) for name, url in m.items()]
        except (ValueError, TypeError):
            pass
    return [(None, _broker_url(None))]


def _next_generation():
    # A monotonic per-host counter (the lease_epoch primary source). Bumped on every grant. The
    # floor is the max of BOTH counter files — our own pls-generation AND the daemon's boot-fence
    # `epoch` file — so a grant issued after a daemon restart is strictly greater than that boot's
    # announced epoch and the two files converge on one monotone series (issue #4). We only READ
    # `epoch`; the daemon is its sole writer (and it floors at this file symmetrically).
    state = _state_dir()
    path = os.path.join(state, "pls-generation")
    cur = 0
    for p in (path, os.path.join(state, "epoch")):
        try:
            with open(p, "r") as f:
                cur = max(cur, int((f.read() or "0").strip() or "0"))
        except (OSError, ValueError):
            pass
    nxt = cur + 1
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        f.write(str(nxt) + "\n")
    os.replace(tmp, path)
    return nxt


def _http(url, method, path, body=None, timeout=10.0):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url + path, data=data, method=method)
    if data is not None:
        req.add_header("Content-Type", "application/json")
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode() or "{}")


def _flag(args, name, default=None):
    if name in args:
        i = args.index(name)
        if i + 1 < len(args):
            return args[i + 1]
    return default


def cmd_acquire(args):
    pool = _flag(args, "--pool")
    cost = _flag(args, "--cost", "0")
    priority = _flag(args, "--priority", "10")
    tenant = _flag(args, "--tenant", "tally")
    url = _broker_url(pool)
    # Map tally's numeric priority to the upstream priority-name bucket (higher number => higher).
    try:
        pn = int(float(priority))
    except (TypeError, ValueError):
        pn = 10
    prio_name = "high" if pn >= 100 else ("low" if pn <= 10 else "normal")
    resp = _http(url, "POST", "/acquire", {
        "user": tenant, "tool": "tally:{}".format(pool), "cost": float(cost or 0), "priority": prio_name,
    })
    ticket = resp.get("id", "")
    state = resp.get("state")
    if state == "held":
        print(json.dumps({
            "lease_id": ticket, "pool": pool, "generation": _next_generation(),
            "granted": True, "cost": float(cost or 0),
        }))
        return 0
    # waiting / anything-else => both-or-queue: report queued (tally re-drives on a backoff timer).
    # A one-shot `pls acquire` invocation exits right after printing — nothing stays alive to hold
    # this waiting ticket, so it must not survive us. Release it now (best-effort, same as coalloc's
    # rollback below) or it becomes an orphan the broker later promotes to holder with no live
    # process behind it, deadlocking the pool (issue #1).
    try:
        _http(url, "POST", "/release/" + str(ticket), {})
    except (urllib.error.URLError, OSError):
        pass
    print(json.dumps({
        "lease_id": ticket, "pool": pool, "granted": False, "queued": True,
        "position": resp.get("position") or 0,
    }))
    return 0


def cmd_release(args):
    lease = _flag(args, "--lease")
    # We do not know the pool here; use the default broker (releases are addressed by ticket id, and
    # a release of an unknown id is a harmless no-op on the upstream broker).
    url = _broker_url(_flag(args, "--pool"))
    try:
        _http(url, "POST", "/release/" + str(lease), {})
        print(json.dumps({"released": True, "lease_id": lease}))
    except (urllib.error.URLError, OSError):
        # Idempotent-by-id: a release that cannot reach the broker still reports non-fatally.
        print(json.dumps({"released": False, "lease_id": lease}))
    return 0


def cmd_status(args):
    pool = _flag(args, "--pool")
    if pool is None:
        # No --pool: broker.ts (the frozen consumer of this verb) always passes --pool, so this path
        # is never on the machine contract — alias it to the operator-facing multi-pool `list` view
        # (issue #8) instead of guessing a pool.
        return cmd_list(args)
    url = _broker_url(pool)
    s = _http(url, "GET", "/status")
    held = s.get("held") or []
    waiting = s.get("waiting") or []
    print(json.dumps({
        "pool": pool, "capacity": s.get("capacity", 0), "budget": s.get("budget", 0),
        "held": len(held), "queued": len(waiting),
    }))
    return 0


def _fmt(v):
    return v if v is not None else "?"


def _ticket_view(t):
    # Normalize one held/waiting entry from GET /status into {id, age_s, ttl_s}. The upstream
    # broker's exact ticket field names aren't part of tally's frozen contract (only the count-only
    # `status --pool` shape is, above), so read tolerantly across the plausible aliases and fall back
    # to "?" (never crash `list` on a broker that reports a slightly different ticket shape).
    tid = t.get("id") or t.get("ticket") or t.get("lease_id") or "?"
    age = t.get("age_s")
    if age is None:
        age = t.get("age")
    ttl = t.get("ttl_s")
    if ttl is None:
        ttl = t.get("ttl")
    return {"id": tid, "age_s": age, "ttl_s": ttl}


def cmd_list(args):
    # `pls list` (issue #8): per-pool holders + waiting tickets with ids/ages/TTLs — a thin formatting
    # wrapper over the same GET /status cmd_status already calls, just not discarding the ticket
    # contents. --pool narrows to one pool; otherwise every pool this box knows about (_all_pool_urls).
    pool = _flag(args, "--pool")
    as_json = "--json" in args
    targets = [(pool, _broker_url(pool))] if pool else _all_pool_urls()
    results = []
    for name, url in targets:
        try:
            s = _http(url, "GET", "/status")
        except (urllib.error.URLError, OSError) as exc:
            results.append({"pool": name, "error": str(exc)})
            continue
        results.append({
            "pool": name if name is not None else s.get("pool"),
            "capacity": s.get("capacity", 0),
            "budget": s.get("budget", 0),
            "held": [_ticket_view(t) for t in (s.get("held") or [])],
            "waiting": [_ticket_view(t) for t in (s.get("waiting") or [])],
        })
    if as_json:
        print(json.dumps(results))
        return 0
    for r in results:
        label = r["pool"] if r.get("pool") else "(default)"
        if "error" in r:
            print("{}: unreachable ({})".format(label, r["error"]))
            continue
        print("{}  capacity={} budget={} held={} waiting={}".format(
            label, r["capacity"], r["budget"], len(r["held"]), len(r["waiting"])))
        for h in r["held"]:
            print("  held    id={} age={}s ttl={}s".format(h["id"], _fmt(h["age_s"]), _fmt(h["ttl_s"])))
        for w in r["waiting"]:
            print("  waiting id={} age={}s ttl={}s".format(w["id"], _fmt(w["age_s"]), _fmt(w["ttl_s"])))
    return 0


def cmd_coalloc(args):
    # Atomic two-pool co-allocation: acquire both; if either is not immediately held, release the one
    # we got and report queued (both-or-queue, never a partial hold).
    pools = (_flag(args, "--pools", "") or "").split(",")
    costs = (_flag(args, "--costs", "") or "").split(",")
    priority = _flag(args, "--priority", "10")
    tenant = _flag(args, "--tenant", "tally")
    grants = []
    for i, p in enumerate(pools[:2]):
        c = costs[i] if i < len(costs) else "0"
        url = _broker_url(p)
        resp = _http(url, "POST", "/acquire", {"user": tenant, "tool": "tally:{}".format(p), "cost": float(c or 0), "priority": "normal"})
        if resp.get("state") == "held":
            grants.append({"lease_id": resp.get("id", ""), "pool": p, "generation": _next_generation(), "granted": True, "cost": float(c or 0)})
        else:
            # roll back any grant we already hold
            for g in grants:
                try:
                    _http(_broker_url(g["pool"]), "POST", "/release/" + g["lease_id"], {})
                except (urllib.error.URLError, OSError):
                    pass
            # release the just-created waiting ticket too
            try:
                _http(url, "POST", "/release/" + resp.get("id", ""), {})
            except (urllib.error.URLError, OSError):
                pass
            print(json.dumps({"granted": False, "queued": True, "pools": pools[:2]}))
            return 0
    print(json.dumps({"granted": True, "leases": grants, "priority": int(float(priority))}))
    return 0


def main(argv):
    if len(argv) < 1:
        print("usage: pls <acquire|release|status|list|coalloc> ...", file=sys.stderr)
        return 2
    verb = argv[0]
    rest = argv[1:]
    try:
        if verb == "acquire":
            return cmd_acquire(rest)
        if verb == "release":
            return cmd_release(rest)
        if verb == "status":
            return cmd_status(rest)
        if verb == "list":
            return cmd_list(rest)
        if verb == "coalloc":
            return cmd_coalloc(rest)
    except (urllib.error.URLError, OSError) as exc:
        print("pls {}: broker unreachable ({})".format(verb, exc), file=sys.stderr)
        return 1
    print("pls: unknown subcommand '{}'".format(verb), file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
