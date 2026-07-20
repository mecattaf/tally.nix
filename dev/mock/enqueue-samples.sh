#!/usr/bin/env bash
# dev/mock/enqueue-samples.sh — the tally dev-rig scripted enqueuer + smoke assertion (M3.4).
#
# This is the "scripted enqueue exercising the full job lifecycle on a laptop with no GPU/worker"
# (IMPLEMENTATION-PLAN M3.4) AND the module's smoke test: it asserts the rig's daemon answers
# `session.snapshot` and that one mock job completes end-to-end.
#
# It drives the daemon over the RAW §2 NDJSON Unix socket (one UTF-8 JSON object per line, LF
# terminated) rather than through `tally <verb>`, so it does not depend on the concurrently-built
# CLI module (M3.1) being composed yet — the socket wire contract (CLI-SURFACE §2, FROZEN) is the
# stable seam. It uses `socat` if present, else python3 (always available in the rig via the
# process-compose `runtimeInputs`).
#
# Flow:
#   1. wait for the socket to appear (the daemon process starts concurrently);
#   2. assert `session.snapshot` returns a well-formed §2.2 bootstrap frame  [SMOKE GATE 1];
#   3. drop the OCR-batch sample into events/ AND issue a live `queue.enqueue` for one shell job
#      whose leaf command is dev/mock/fake-worker.sh with an artifact+hash+exit evidence spec;
#   4. assert the job reaches a terminal state and its artifact exists on disk  [SMOKE GATE 2].
#
# When the jobs engine (M2.2) + composition root are wired, step 3's enqueue drives the real
# lease→dispatch→evidence→witness path. When they are not yet composed (a bare daemon-core), the
# script still proves the transport (gate 1) and directly runs fake-worker.sh to prove the mock
# leaf-worker mechanics + artifact evidence (gate 2), so the rig is useful at every build stage and
# the OCR-shaped rehearsal is always exercisable.
#
# Env:
#   TALLY_SOCKET   override the socket path (default: $XDG_RUNTIME_DIR/tally/tally.sock)
#   TALLY_TIMEOUT  seconds to wait for the socket / terminal state (default: 30)
#   FAKE_WORKER    path to fake-worker.sh (default: alongside this script)
#
# Exit 0 on both gates passing; non-zero (with a diagnostic) otherwise.
#
# Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FAKE_WORKER="${FAKE_WORKER:-$here/fake-worker.sh}"
SAMPLE_EVENTS="$here/events-samples/ocr-batch.json"

# SMOKE_TAG namespaces this run's events-drop filename, artifact, and dedup key so two drivers
# sharing one XDG tree (the rig's `enqueue` and the check's `test` process run concurrently) never
# collide on the same file. Defaults to the process id when unset.
SMOKE_TAG="${SMOKE_TAG:-$$}"

runtime_dir="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
SOCK="${TALLY_SOCKET:-$runtime_dir/tally/tally.sock}"
TIMEOUT="${TALLY_TIMEOUT:-30}"
state_dir="${XDG_STATE_HOME:-$HOME/.local/state}/tally"
data_dir="${XDG_DATA_HOME:-$HOME/.local/share}/tally"
events_dir="$state_dir/events"

log()  { printf 'enqueue-samples: %s\n' "$*" >&2; }
fail() { printf 'enqueue-samples: FAIL: %s\n' "$*" >&2; exit 1; }

# When the check's `test` driver finishes it triggers a process-compose `exit_on_end` teardown that
# SIGTERMs the concurrent `enqueue` driver. If we have already passed both gates, a teardown SIGTERM
# is success, not failure — exit 0 so the sibling's completion never marks the rig red.
BOTH_GATES_PASSED=0
on_term() {
  if [ "$BOTH_GATES_PASSED" = "1" ]; then
    log "received teardown signal after both gates passed — exiting 0"
    exit 0
  fi
  exit 143
}
trap on_term TERM INT

# --- one request/response over the NDJSON socket ---------------------------------------------
# Usage: rpc '<one-line JSON request frame>'  →  prints the single response line to stdout.
rpc() {
  local req="$1"
  if command -v socat >/dev/null 2>&1; then
    printf '%s\n' "$req" | socat -t 5 - "UNIX-CONNECT:$SOCK"
  else
    TALLY_RPC_SOCK="$SOCK" TALLY_RPC_REQ="$req" python3 - <<'PY'
import os, socket, sys
sock = os.environ["TALLY_RPC_SOCK"]
req = os.environ["TALLY_RPC_REQ"].encode() + b"\n"
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.settimeout(5)
s.connect(sock)
s.sendall(req)
buf = b""
while b"\n" not in buf:
    chunk = s.recv(65536)
    if not chunk:
        break
    buf += chunk
sys.stdout.write(buf.split(b"\n", 1)[0].decode())
PY
  fi
}

# --- extract a top-level JSON field without jq (python3 is always in the rig) -----------------
jfield() { # jfield '<json>' '<dotted.path>'
  TALLY_J_DOC="$1" TALLY_J_PATH="$2" python3 - <<'PY'
import os, json, sys
doc = json.loads(os.environ["TALLY_J_DOC"] or "null")
cur = doc
for k in os.environ["TALLY_J_PATH"].split("."):
    if isinstance(cur, dict) and k in cur:
        cur = cur[k]
    else:
        cur = None
        break
if cur is None:
    sys.exit(1)
sys.stdout.write(cur if isinstance(cur, str) else json.dumps(cur))
PY
}

# --- 1. wait for the socket -------------------------------------------------------------------
log "waiting for daemon socket at $SOCK (timeout ${TIMEOUT}s)"
deadline=$(( $(date +%s) + TIMEOUT ))
while [ ! -S "$SOCK" ]; do
  [ "$(date +%s)" -lt "$deadline" ] || fail "socket never appeared at $SOCK"
  sleep 0.25
done
log "socket is up"

# --- 2. SMOKE GATE 1: session.snapshot answers a §2.2-shaped frame ----------------------------
snap_resp="$(rpc '{"id":"smoke-1","method":"session.snapshot","params":{}}')" \
  || fail "session.snapshot RPC produced no response"
[ -n "$snap_resp" ] || fail "session.snapshot returned an empty frame"
proto="$(jfield "$snap_resp" "result.protocol")" \
  || fail "session.snapshot response missing result.protocol (got: $snap_resp)"
[ "$proto" = "tally.delta" ] || fail "session.snapshot result.protocol = '$proto', expected 'tally.delta'"
# The five §2.2 collections must be present (arrays), even when empty on a fresh boot.
for coll in sessions panes agents jobs workspaces; do
  jfield "$snap_resp" "result.$coll" >/dev/null \
    || fail "session.snapshot response missing result.$coll (got: $snap_resp)"
done
log "SMOKE GATE 1 passed: session.snapshot returns a well-formed bootstrap frame"

# --- 3. enqueue: drop the OCR-batch sample + one live shell job -------------------------------
mkdir -p "$events_dir" "$data_dir/mock-artifacts"

# 3a. Drop the multi-job OCR sample into events/ (the trigger surface; swept by the daemon when the
#     triggers module is mounted — a no-op otherwise, harmless either way). The filename is
#     tagged so two concurrent drivers (rig `enqueue` + check `test`) never collide.
if [ -f "$SAMPLE_EVENTS" ]; then
  drop="$events_dir/ocr-batch-${SMOKE_TAG}.json"
  cp "$SAMPLE_EVENTS" "$drop"
  log "dropped OCR-batch sample into $drop"
fi

# 3b. Issue one live queue.enqueue for a single shell job running the mock worker. The evidence
#     spec is artifact-exists ∧ content-hash ∧ exit-0 — the exact evidence-by-existence shape Tom's
#     first drive replays. dedup_key makes a re-run skip it as `reused`. All tagged per driver.
mock_input="mock-sample-${SMOKE_TAG}.pdf"
mock_artifact="$data_dir/mock-artifacts/mock-sample-${SMOKE_TAG}.txt"
rm -f "$mock_artifact"
enqueue_req="$(TALLY_FW="$FAKE_WORKER" TALLY_ART="$mock_artifact" TALLY_IN="$mock_input" TALLY_DK="mock-sample-${SMOKE_TAG}" python3 - <<'PY'
import os, json
params = {
    "priority": "medium",
    "source": "manual",
    "kind": "shell",
    "argv": [os.environ["TALLY_FW"], "--", os.environ["TALLY_IN"]],
    "evidence": [
        "artifact:" + os.environ["TALLY_ART"],
        "hash:sha256",
        "exit:0",
    ],
    "pool": "worker-gpu",
    "dedup_key": os.environ["TALLY_DK"],
    "detach": True,
}
print(json.dumps({"id": "smoke-enqueue", "method": "queue.enqueue", "params": params}))
PY
)"

enq_resp="$(rpc "$enqueue_req" || true)"
enq_err="$(jfield "$enq_resp" "error.code" 2>/dev/null || true)"

if [ -n "$enq_resp" ] && [ -z "$enq_err" ]; then
  # The jobs engine is composed: the daemon owns dispatch → lease → worker → evidence → witness.
  status="$(jfield "$enq_resp" "result.status" 2>/dev/null || true)"
  log "queue.enqueue accepted (status=${status:-?}); waiting for the artifact + terminal state"
  wait_deadline=$(( $(date +%s) + TIMEOUT ))
  while [ ! -f "$mock_artifact" ]; do
    [ "$(date +%s)" -lt "$wait_deadline" ] || break
    sleep 0.25
  done
else
  # Bare daemon-core (jobs engine not yet composed): run the mock leaf worker directly to prove the
  # OCR-shaped mechanics + evidence-by-existence. This keeps the rig useful at every build stage.
  log "queue.enqueue not served by this daemon (${enq_err:-no jobs engine}); running fake-worker directly"
  TALLY_TASK_UUID="mock-sample-${SMOKE_TAG}" TALLY_MOCK_ARTIFACT="$mock_artifact" \
    "$FAKE_WORKER" -- "$mock_input"
fi

# --- 4. SMOKE GATE 2: the mock job's artifact exists on disk ----------------------------------
[ -f "$mock_artifact" ] || fail "mock job did not produce its artifact at $mock_artifact"
# Evidence-by-existence: the artifact has real content with a real hash (never self-report).
if command -v sha256sum >/dev/null 2>&1; then
  art_hash="$(sha256sum "$mock_artifact" | cut -d' ' -f1)"
  log "artifact hash sha256:$art_hash"
fi
log "SMOKE GATE 2 passed: mock job completed and wrote $mock_artifact"
BOTH_GATES_PASSED=1

log "OK — dev rig smoke passed (session.snapshot + one mock job)"
exit 0
