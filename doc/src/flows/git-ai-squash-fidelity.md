# git-ai squash fidelity

Campaign merges are squashes by default. `AUGUST-01-DESIGN.md` §7 records the
hard interaction that follows — "notes do not survive squash-merge" — and §9.3.2
rules that the first step of arming git-ai is not a design but a measurement:
does a squash commit carry **per-line** attribution, or only a summary?

`test/git-ai-squash-fidelity.sh` is that measurement. It is evidence, not a gate:
it needs the externally provisioned `git-ai` binary that the flake deliberately
does not package, so it skips with exit 0 when the binary is absent and nothing
in `nix flake check` depends on it.

```console
$ test/git-ai-squash-fidelity.sh --out /tmp/spike
```

Each scenario builds the same working branch — an assisted line, an unassisted
line, and a second assisted line from a second session — squashes it onto the
base branch a different way, and reports what the squash commit carries.

## Recorded findings (git-ai 1.6.17, 2026-08-02)

| Scenario | Note after `git-ai await` | Note after a git-ai read | Attribution | Attributed lines |
|---|---|---|---|---|
| `local-squash` — squash runs in the clone that holds the notes | yes | yes | per-line | 2, 5 |
| `remote-squash` — squash runs elsewhere and is fetched | no | no | none | — |
| `pruned-source` — same, after the branch and its notes are deleted | no | no | none | — |
| `fresh-clone` — base branch plus `refs/notes/ai`, nothing else | yes | yes | per-line | 2, 5 |

The `local-squash` note, verbatim:

```
a.txt
  s_80c295a0a07e14::t_a4858379ad1029 2
  s_02ad0fb699a004::t_9db43c59e14383 5
---
{
  "schema_version": "authorship/3.0.0",
  "git_ai_version": "1.6.17",
  "base_commit_sha": "55a804dc27801fc321dc07453b007f6254e74ee3",
  "prompts": {},
  "sessions": {
    "s_02ad0fb699a004": { "agent_id": { "tool": "mock_ai", ... } },
    "s_80c295a0a07e14": { "agent_id": { "tool": "mock_ai", ... } }
  }
}
```

## What this means

1. **Fidelity is per-line, not summary-only.** The squash commit's note names
   exact line numbers and maps each to the session that produced it. Both
   sessions from the working branch survive as distinct entries, and the
   unassisted line 4 is correctly absent — the mixture is preserved, not
   collapsed into a commit-level ratio.
2. **§7's "notes do not survive squash" is true of the *ref*, not of the
   *attribution*.** No note is copied from the pre-squash commits. A new note is
   **re-minted** for the squash commit by git-ai's background service, which
   observes `git commit` through the global `trace2.eventTarget` that
   `git-ai install-hooks` writes — not through repository hooks. The service
   recovers per-line attribution by content-matching the squashed hunks against
   the checkpoints it already holds.
3. **Re-minting happens only where the squash is executed.** `remote-squash` is
   the decisive negative result: when GitHub performs the squash and tally merely
   fetches the resulting commit, no note appears — even though the noted working
   commits are still present in the local object store. The binding is a
   commit-time event, not a fetch-time or read-time one, so a `gh pr merge
   --squash` produces an unbound squash commit and `git-ai stats` reports every
   added line as `unknown_additions`.
4. **The note is publishable and travels.** `fresh-clone` fetches only the base
   branch and `refs/notes/ai` and reads the full per-line note. Notes are not
   pushed by default, so publication is an explicit `git push <remote>
   refs/notes/ai:refs/notes/ai`.

## What the binding built on this (car F)

These findings are the whole design of the merge node's post-merge binding,
which `gitAiBinding` arms (`doc/src/flows/campaigns.md`). Point by point:

- Finding 3 is why the binding **re-mints the squash locally instead of asking
  for it**. Nothing recovers attribution from a commit made elsewhere, so the
  merge node squashes the same head onto the same base a second time in a
  detached worktree of the campaign checkout, and copies the resulting note
  onto the integrated commit's object ID.
- The consequence of copying is that the reconstruction must be proven to be
  the same content, not merely similar work. The node requires the integrated
  commit's first parent to be the gated base and its tree to equal the
  reconstruction's tree before any note is copied; a mismatch is refused with
  that status and nothing is written. Finding 1 is what makes the copy honest:
  the note names exact lines against an exact tree, and an identical tree on an
  identical parent has an identical diff.
- Finding 2 is why the reconstruction happens in a worktree of the campaign
  checkout. The service observes `git commit` through a global
  `trace2.eventTarget`, and it recovers attribution by content-matching against
  checkpoints it already holds. `pruned-source` is what a binding that runs
  after branch cleanup gets: nothing. Lanes are `git worktree`s of the campaign
  checkout and share one object store and one `refs/notes/ai`, so that is where
  the binding runs.
- Finding 4 is why publication is an explicit `git push <remote>
  refs/notes/ai:refs/notes/ai` step, and why it is worth doing: the note
  travels, and a clone with only the base branch and the notes ref reads full
  per-line attribution.
- The unarmed-host ambiguity is why `mode = "required"` and
  `gitAiBinding = "required"` both stay off for this wave. A host that never
  had the trace2 target installed and a squash that lost its attribution
  produce the same evidence, so `required` cannot tell a real authorship
  failure from an unprovisioned host until real squash merges have shown the
  binding working.
- The `Assisted-by: <adapter>:<model> (tally:<taskUuid> witness:<seq>)` trailer
  stays a pointer. The note is the proof, and per finding 4 the proof publishes
  independently of the trailer.

One operational number, measured on the same host: `git-ai await` costs roughly
18 seconds on a repository with nothing outstanding, because it waits on the
background service rather than on the note. That is spent inside the merge node
on every bound task, and it is a reason the posture is per-campaign rather than
fleet-wide.
