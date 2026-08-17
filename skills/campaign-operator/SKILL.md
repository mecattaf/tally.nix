---
name: campaign-operator
description: Observe, steer, release, or abandon an armed local tally campaign. Use when operating a campaign after tally campaign arm, inspecting its live state, supplying local steering, handling an escalation, or publishing a completed campaign.
---

# Operate a silent tally campaign

Identify a campaign by the same `OWNER/REPO` and committed worklist pattern used
to arm it. Keep coordination local until the one release act.

## Observe

Start with the campaign view:

```text
tally campaign status OWNER/REPO PATH/TO/WORKLIST.json
```

Use its latest flow-run ID for task detail or transition history:

```text
tally query run FLOW_RUN_ID
tally query log --flow-run FLOW_RUN_ID
```

Prefer `--json` for automation. Treat `campaign status` and `tally query` as the
supported read surface. If the live read path is unavailable, use `tally rebuild`
to derive the durable view from local stores and unit facts; do not reconstruct
campaign state by comparing secondary surfaces. Continue until status is complete
or escalated.

Attribute every observation against the deployed machinery. A campaign is
graded by the flow and driver on the deployed pin, not by the bytes in the
tree: merging a change to them alters nothing until the next deploy, and a
campaign that has started keeps the store path it started under for its whole
life. Read what is actually running before crediting or blaming any merged
commit:

```text
tally campaign quiescent
```

The verb succeeds only when no campaign is armed; otherwise it prints every
registration, `flow` and `driver` store path included, and fails. Diff that
store path against the commit in question first. A live behavior credited to
code the running pin does not carry is a false finding, and this mistake
invalidated two findings in a single closing record.

## Steer

Steer only when the live view identifies a missing outcome or a blocked attempt.
State the observed gap and required result; leave implementation choices to the
worker.

Address one task:

```text
tally campaign steer OWNER/REPO PATH/TO/WORKLIST.json \
  --task TASK_ID --message 'Required outcome and evidence'
```

Omit `--task` for campaign-wide guidance. For SSH, send the text on stdin so it
is neither exposed nor re-quoted in the remote argv:

```text
printf '%s\n' 'Required outcome and evidence' | \
  ssh HOST tally campaign steer OWNER/REPO PATH/TO/WORKLIST.json \
    --task TASK_ID --message-file -
```

Steering is append-only and enters the next attempt through tally. Do not edit
receipts, worktrees, integration branches, or the approved graph by hand.

Steering is also the whole recovery path. Attempt budgets are derived over
epochs: a receipt counts only while it matches the task's current input — its
bytes, its gates, and the steering addressed to it — so correcting the cause
and steering the task moves its epoch and refreshes its budget. There is no
resume verb and no pardon to record; do not look for either.

To change the campaign contract, edit the worklist, merge and push it, then run
`tally campaign arm` again to approve the new graph.

Some states are not steerable. A campaign that cannot proceed without a hand
edit to receipts, refs, or a worktree is failure weather: record the state and
stop it there. Leave the forensics in place, clean nothing up, and hand the
ruling to the operator. A structural improvisation outside the armed graph is
one failed lane away from the same deadlock.

## Release

Release only after `campaign status` reports complete. Invoke the release
renderer once with the campaign identity:

```text
tally campaign release OWNER/REPO PATH/TO/WORKLIST.json
```

Let that verb validate history, publish the integrated result, render the
operator-authored project intent verbatim into the sparse issue, render and merge
the review stack, write the receipts summary, and close the issue. Do not perform
its component steps manually. The verb is idempotent: after an interruption,
rerun the identical command.

Exercise release probe mode only against a private `tally-probe-*` target. Let
the verb own creation, teardown, and expiry of that target.

Until the close is a single verb, run it as an ordered checklist and take no
step out of order:

1. `tally campaign quiescent` — read what is armed and what is grading it.
2. `spec-lint --coverage specs/<identity>` — when the campaign has a governing
   spec, render the claim ↔ task ↔ acceptance ↔ evidence table, review it, and
   hand it to release as part of the operator-authored intent. Release renders
   that intent verbatim, so the rendered table is the close-out proof; never
   retype it by hand.
3. `tally campaign release --plan` — render the complete close without
   contacting a forge, and read what it renders.
4. `tally campaign release --probe` — exercise it against the disposable
   target.
5. `tally campaign release` — the one release act.
6. `tally campaign disarm` — last, and nothing after it.

## Abandon

If the operator deliberately cancels the campaign, remove its registration with:

```text
tally campaign disarm OWNER/REPO PATH/TO/WORKLIST.json
```

Disarm is terminal: registration-scoped state, steering included, does not
survive it, and a disarm taken before release costs a re-arm and hand-restored
refs. Do not use disarm as failure recovery.
