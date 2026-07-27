#!/usr/bin/env bash
set -euo pipefail

artifact="${1:?the packaged book path is required}"
shift

dry_run=false

usage() {
  echo "usage: tally-publish-docs [--dry-run]"
}

case "${1:-}" in
  "")
    ;;
  --dry-run)
    dry_run=true
    shift
    ;;
  -h|--help)
    usage
    exit 0
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac

if [[ "$#" -ne 0 ]]; then
  usage >&2
  exit 2
fi

if [[ ! -f "$artifact/index.html" ]]; then
  echo "packaged book has no index.html: $artifact" >&2
  exit 1
fi

repository="$(git rev-parse --show-toplevel)"
cd "$repository"

if [[ -n "$(git status --porcelain=v1 --untracked-files=normal)" ]]; then
  echo "refusing to publish from a dirty worktree" >&2
  exit 1
fi

remote_url="$(git remote get-url origin)"
source_commit="$(git rev-parse HEAD)"
source_author_name="$(git show -s --format=%an HEAD)"
source_author_email="$(git show -s --format=%ae HEAD)"
remote_main_ref="$(git ls-remote --heads "$remote_url" refs/heads/main)"
remote_main="${remote_main_ref%%[[:space:]]*}"

if [[ -z "$remote_main" ]]; then
  echo "origin has no main branch" >&2
  exit 1
fi

if [[ "$dry_run" == false && "$source_commit" != "$remote_main" ]]; then
  echo "refusing to publish a source commit other than origin/main" >&2
  echo "source: $source_commit" >&2
  echo "origin/main: $remote_main" >&2
  exit 1
fi

scratch="$(mktemp -d /tmp/tally-doc-publish.XXXXXX)"
cleanup() {
  case "$scratch" in
    /tmp/tally-doc-publish.*)
      rm -rf -- "$scratch"
      ;;
    *)
      echo "refusing to clean unexpected temporary path: $scratch" >&2
      ;;
  esac
}
trap cleanup EXIT

pages_checkout="$scratch/gh-pages"
git init --quiet "$pages_checkout"
git -C "$pages_checkout" remote add origin "$remote_url"

pages_ref="$(git ls-remote --heads "$remote_url" refs/heads/gh-pages)"
if [[ -n "$pages_ref" ]]; then
  git -C "$pages_checkout" fetch --quiet --depth 1 origin gh-pages
  git -C "$pages_checkout" checkout --quiet -b gh-pages FETCH_HEAD
else
  git -C "$pages_checkout" switch --quiet --orphan gh-pages
fi

rsync \
  --archive \
  --delete \
  --chmod=D=u+rwx,go+rx,F=u+rw,go+r \
  --exclude='/.git/' \
  "$artifact/" \
  "$pages_checkout/"
printf '%s\n' "$source_commit" >"$pages_checkout/.tally-doc-source"

git -C "$pages_checkout" config user.name "$source_author_name"
git -C "$pages_checkout" config user.email "$source_author_email"
git -C "$pages_checkout" add --all

if git -C "$pages_checkout" diff --cached --quiet; then
  echo "gh-pages already contains the packaged book for $source_commit"
  exit 0
fi

git -C "$pages_checkout" commit --quiet -m "docs: publish $source_commit"
publication_commit="$(git -C "$pages_checkout" rev-parse HEAD)"

if [[ "$dry_run" == true ]]; then
  echo "dry run: prepared gh-pages commit $publication_commit from $source_commit"
  exit 0
fi

git -C "$pages_checkout" push origin HEAD:gh-pages
echo "published $source_commit as gh-pages commit $publication_commit"
