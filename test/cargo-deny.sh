#!/usr/bin/env bash
set -euo pipefail

if [[ -z "${TALLY_ADVISORY_DB_REPOSITORY:-}" ]]; then
  echo "TALLY_ADVISORY_DB_REPOSITORY is unset; run this check through nix develop" >&2
  exit 2
fi

advisory_root="$(mktemp -d)"
trap 'rm -rf -- "$advisory_root"' EXIT

# cargo-deny derives this stable directory name from the configured RustSec URL.
# Copying also gives Git a same-owner repository instead of a Nix-store symlink.
advisory_repository="$advisory_root/advisory-db-3157b0e258782691"
mkdir -p "$advisory_repository"
cp -R "$TALLY_ADVISORY_DB_REPOSITORY"/. "$advisory_repository/"
chmod -R u+w "$advisory_repository"
export TALLY_ADVISORY_DB_ROOT="$advisory_root"

printf 'cargo-deny advisory database: %s\n' "${TALLY_ADVISORY_DB_REVISION:-unknown}"
cargo deny --offline --locked check --hide-inclusion-graph "$@"
