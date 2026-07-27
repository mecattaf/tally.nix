#!/usr/bin/env bash
set -euo pipefail

source_dir="${1:-src}"
summary="$source_dir/SUMMARY.md"

if [[ ! -f "$summary" ]]; then
  echo "missing mdBook summary: $summary" >&2
  exit 1
fi

scratch="$(mktemp -d)"
cleanup() {
  rm -rf -- "$scratch"
}
trap cleanup EXIT

all_pages="$scratch/all-pages"
summary_pages="$scratch/summary-pages"

find "$source_dir" -type f -name '*.md' ! -name 'SUMMARY.md' -printf '%P\n' \
  | LC_ALL=C sort -u >"$all_pages"

sed -nE \
  's@^[[:space:]]*([-*][[:space:]]+)?\[[^]]+\]\(([^)#]+)(#[^)]*)?\)[[:space:]]*$@\2@p' \
  "$summary" \
  | sed 's@^\./@@' \
  | LC_ALL=C sort >"$summary_pages"

status=0

duplicates="$(uniq -d "$summary_pages")"
if [[ -n "$duplicates" ]]; then
  echo "pages listed more than once in SUMMARY.md:" >&2
  printf '%s\n' "$duplicates" >&2
  status=1
fi

missing="$(comm -13 "$all_pages" "$summary_pages")"
if [[ -n "$missing" ]]; then
  echo "SUMMARY.md references missing pages:" >&2
  printf '%s\n' "$missing" >&2
  status=1
fi

orphans="$(comm -23 "$all_pages" "$summary_pages")"
if [[ -n "$orphans" ]]; then
  echo "pages not reachable from SUMMARY.md:" >&2
  printf '%s\n' "$orphans" >&2
  status=1
fi

if [[ "$status" -ne 0 ]]; then
  exit "$status"
fi

page_count="$(wc -l <"$all_pages")"
echo "SUMMARY.md reaches all $page_count book pages"
