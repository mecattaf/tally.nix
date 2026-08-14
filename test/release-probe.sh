#!/usr/bin/env bash
# Named pre-release operator step for the one real GitHub probe. Automated
# campaign lanes exercise the same lifecycle with --gh-program pointed at a
# local recording shim; this wrapper deliberately fixes that program to `gh`.
set -euo pipefail

usage() {
  printf 'usage: test/release-probe.sh OWNER/REPO WORKLIST [--state-dir PATH]\n' >&2
  exit 2
}

[[ $# -ge 2 ]] || usage
readonly source_repository="$1"
readonly worklist="$2"
shift 2

while [[ $# -gt 0 ]]; do
  case "$1" in
    --state-dir)
      [[ $# -ge 2 ]] || usage
      readonly state_dir="$2"
      shift 2
      ;;
    *) usage ;;
  esac
done

command -v gh >/dev/null 2>&1 || {
  printf 'release probe: the real GitHub CLI (`gh`) is required\n' >&2
  exit 2
}

readonly repository_root="$(git rev-parse --show-toplevel)"
release_args=(
  campaign release "$source_repository" "$worklist"
  --probe
  --gh-program gh
)
if [[ -n "${state_dir:-}" ]]; then
  release_args+=(--state-dir "$state_dir")
fi

if [[ -n "${TALLY_BIN:-}" ]]; then
  [[ -x "$TALLY_BIN" ]] || {
    printf 'release probe: TALLY_BIN is not executable: %s\n' "$TALLY_BIN" >&2
    exit 2
  }
  exec "$TALLY_BIN" "${release_args[@]}"
fi

cd "$repository_root"
exec cargo run --quiet --package tally -- "${release_args[@]}"
