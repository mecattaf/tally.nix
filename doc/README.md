# Documentation infrastructure

The user-facing book is an mdBook rooted at `doc/`. Its source navigation is
`src/SUMMARY.md`; every Markdown page beneath `src/` must appear there exactly once.

Build the same checked artifact exposed by the flake:

```console
$ nix build .#doc
$ xdg-open result/index.html
```

The build runs mdBook with the pinned `mdbook-linkcheck2` backend and rejects broken internal
links, missing pages, duplicate summary entries, and pages orphaned from `SUMMARY.md`.
`checks.<system>.doc` is the same derivation, so `nix flake check` cannot pass with a different
book than `packages.<system>.doc`.

## Publishing

The public site is served directly from this repository's `gh-pages` branch at
<https://mecattaf.github.io/tally.nix/>. The branch contains only the output of `packages.doc`;
the repository does not use a GitHub Actions workflow to build or deploy it.

After a documentation change reaches `origin/main`, publish that exact source commit with:

```console
$ nix run .#publish-docs
```

The publisher refuses a dirty worktree or a real publication from any commit other than the
current remote `main`. It creates a normal descendant commit on `gh-pages`, so concurrent or
non-fast-forward publication attempts fail loudly. Exercise the complete build and branch
assembly without pushing via:

```console
$ nix run .#publish-docs -- --dry-run
```
