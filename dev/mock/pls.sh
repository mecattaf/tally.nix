#!/usr/bin/env bash
# dev/mock/pls.sh — the tally dev-rig MOCK pls broker (M3.4 dev-rig; the "laptop with no GPU" path).
#
# The dev rig has no real pls broker and no GPU (nix/dev.nix: "No GPU/worker/pls"). tally acquires a
# pls lease BEFORE every heavy unit touches the GPU (SPEC "acquire the pls lease before it touches the
# GPU") — even a shell mock job routes through `pls acquire`. So the rig needs a `pls` on PATH that
# ALWAYS grants immediately: the no-contention single-operator laptop case. This mock models the
# documented pls CLI surface (src/pls/broker.ts) faithfully enough for the lease→dispatch→evidence→
# witness path to run end-to-end against the mock leaf worker.
#
# Surface (the four verbs the broker binds):
#   pls acquire --pool <p> --cost <c> --priority <n> [--tenant <t>]
#        -> {"lease_id","pool","generation","granted":true,"cost"}    (always a free slot here)
#   pls release --lease <id>   -> {"released":true,"lease_id"}
#   pls status  --pool <p>     -> {"pool","capacity","budget","held":0,"queued":0,"free_cost"}
#   pls coalloc --pools p1,p2 --costs c1,c2 --priority <n> [--tenant <t>]
#        -> {"granted":true,"leases":[grant,grant],"priority"}
#
# `generation` is a monotone counter (the lease_epoch source, PS#21) kept in a per-rig state file so it
# advances across acquires within one daemon lifetime. stdlib only (bash + coreutils); no jq.
#
# Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).
set -euo pipefail

verb="${1:-}"
shift || true

# Parse the flags the broker passes (order-independent).
pool="worker-gpu"
cost="1"
priority="0"
lease=""
pools=""
costs=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --pool)     pool="${2:-}"; shift 2 ;;
    --cost)     cost="${2:-1}"; shift 2 ;;
    --priority) priority="${2:-0}"; shift 2 ;;
    --tenant)   shift 2 ;;
    --lease)    lease="${2:-}"; shift 2 ;;
    --pools)    pools="${2:-}"; shift 2 ;;
    --costs)    costs="${2:-}"; shift 2 ;;
    *)          shift ;;
  esac
done

# A monotone generation counter (the lease_epoch source). Kept under the rig's runtime dir so it
# advances across acquires within one daemon lifetime; falls back to a nanosecond stamp if unwritable.
gen_file="${XDG_RUNTIME_DIR:-/tmp}/tally/pls-mock-generation"
next_generation() {
  local g=0
  if [ -f "$gen_file" ]; then g="$(cat "$gen_file" 2>/dev/null || echo 0)"; fi
  g=$((g + 1))
  if ! { mkdir -p "$(dirname "$gen_file")" 2>/dev/null && printf '%s' "$g" > "$gen_file" 2>/dev/null; }; then
    g="$(date +%s%N)"
  fi
  printf '%s' "$g"
}

case "$verb" in
  acquire)
    gen="$(next_generation)"
    lid="mock-lease-$gen"
    printf '{"lease_id":"%s","pool":"%s","generation":%s,"granted":true,"cost":%s}\n' "$lid" "$pool" "$gen" "$cost"
    ;;
  release)
    printf '{"released":true,"lease_id":"%s"}\n' "$lease"
    ;;
  status)
    # A free single-slot pool with a generous budget (matches the compiled-in defaults).
    printf '{"pool":"%s","capacity":1,"budget":128,"held":0,"queued":0,"free_cost":128}\n' "$pool"
    ;;
  coalloc)
    p1="${pools%%,*}"; p2="${pools##*,}"
    g1="$(next_generation)"; g2="$(next_generation)"
    printf '{"granted":true,"leases":[{"lease_id":"mock-lease-%s","pool":"%s","generation":%s,"granted":true,"cost":1},{"lease_id":"mock-lease-%s","pool":"%s","generation":%s,"granted":true,"cost":1}],"priority":%s}\n' \
      "$g1" "$p1" "$g1" "$g2" "$p2" "$g2" "$priority"
    ;;
  *)
    printf 'mock pls: unknown verb "%s"\n' "$verb" >&2
    exit 2
    ;;
esac
