#!/usr/bin/env bash
# dev/mock/fake-worker.sh — a mock leaf-worker for the tally dev rig (M3.4 dev-rig).
#
# This stands in for the heavy GPU worker command a real `tally enqueue --kind shell` job would
# run under a pls lease on the headless worker box. On a laptop with NO GPU and NO worker, the dev
# rig dispatches THIS instead: it does exactly what a compliant leaf worker does and nothing tally
# needs to observe out-of-band —
#
#   1. announces start on stdout (journald-shaped so the rig's logs read like production);
#   2. simulates work (a short sleep, honoring $TALLY_MOCK_WORK_MS);
#   3. writes the declared artifact to $TALLY_MOCK_ARTIFACT (so the daemon's evidence gate finds a
#      real file on disk with a real content hash — evidence-by-existence, never self-report);
#   4. exits with $TALLY_MOCK_EXIT (default 0) so the evidence `exit:<code>` check passes.
#
# It is a pure leaf worker: it does NOT write the witness ledger (the daemon's jobs engine owns the
# witness line from the evidence gate), does NOT touch the socket, and does NOT know tally exists
# beyond reading the TALLY_* env the dispatcher sets. That is the whole point — a mock job is
# indistinguishable from a real one to everything above the lease.
#
# Env (all optional, sane defaults so it runs standalone):
#   TALLY_TASK_UUID      the job's uuid (for log correlation; the dispatcher sets it)
#   TALLY_SESSION_REF    the bound session ref (shell kind has none; left blank)
#   TALLY_MOCK_ARTIFACT  absolute path of the artifact to write (default: a temp sidecar)
#   TALLY_MOCK_CONTENT   the artifact body (default: a deterministic OCR-sidecar-shaped stub)
#   TALLY_MOCK_WORK_MS   simulated work duration in milliseconds (default: 150)
#   TALLY_MOCK_EXIT      the exit code to return (default: 0; set non-zero to rehearse failure)
#
# Positional args after `--` are treated as the "OCR input" name, used only to shape the log line
# and the default artifact path, so `fake-worker.sh -- paper-0421.pdf` reads like the real drive.
#
# Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).
set -euo pipefail

# --- resolve the "input" name (everything after a `--`, else $1, else a default) --------------
input=""
seen_ddash=0
for a in "$@"; do
  if [ "$seen_ddash" = "1" ]; then input="$a"; break; fi
  if [ "$a" = "--" ]; then seen_ddash=1; fi
done
if [ -z "$input" ] && [ "$#" -gt 0 ] && [ "$1" != "--" ]; then input="$1"; fi
[ -n "$input" ] || input="mock-input"

task_uuid="${TALLY_TASK_UUID:-mock-$(date +%s)-$$}"
session_ref="${TALLY_SESSION_REF:-}"
work_ms="${TALLY_MOCK_WORK_MS:-150}"
exit_code="${TALLY_MOCK_EXIT:-0}"

# Default artifact under the rig's data dir so the daemon and the smoke can both find it.
default_dir="${XDG_DATA_HOME:-$HOME/.local/share}/tally/mock-artifacts"
artifact="${TALLY_MOCK_ARTIFACT:-$default_dir/${input##*/}.txt}"
mkdir -p "$(dirname "$artifact")"

# Deterministic sidecar body (stable so re-runs hash identically → dedup-by-existence works).
content="${TALLY_MOCK_CONTENT:-mock OCR sidecar for ${input##*/} (task ${task_uuid})}"

# --- 1. announce start (journald-shaped single line on stdout) --------------------------------
printf '%s\n' "$(cat <<JSON
{"SYSLOG_IDENTIFIER":"tally-mock-worker","TALLY_EVENT":"started","TALLY_TASK_UUID":"${task_uuid}","TALLY_SESSION_REF":"${session_ref}","MESSAGE":"mock worker starting: ${input##*/}"}
JSON
)"

# --- 2. simulate work -------------------------------------------------------------------------
# `sleep` takes seconds; convert ms. Keep it short so the rig feels live.
sleep_s="$(awk "BEGIN { printf \"%.3f\", ${work_ms} / 1000 }")"
sleep "$sleep_s"

# --- 3. write the artifact (the evidence the daemon's gate will stat + hash) -------------------
printf '%s\n' "$content" > "$artifact"

# --- 4. report completion + exit with the declared code ---------------------------------------
printf '%s\n' "$(cat <<JSON
{"SYSLOG_IDENTIFIER":"tally-mock-worker","TALLY_EVENT":"completed","TALLY_TASK_UUID":"${task_uuid}","TALLY_ARTIFACT":"${artifact}","TALLY_EXIT_CODE":${exit_code},"MESSAGE":"mock worker wrote ${artifact} (exit ${exit_code})"}
JSON
)"

exit "$exit_code"
