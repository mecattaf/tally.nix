---
name: campaign-operator
description: Observe, steer, resume, release, or abandon an armed local tally campaign. Use when operating a campaign after tally campaign arm, inspecting its live state, supplying local steering, handling an escalation, or publishing a completed campaign.
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

After correcting the cause of an escalation, record the pardon and resume:

```text
tally campaign resume OWNER/REPO PATH/TO/WORKLIST.json \
  --reason 'What changed and why another attempt is sound'
```

To change the campaign contract, edit the worklist, merge and push it, then run
`tally campaign arm` again to approve the new graph.

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

## Abandon

If the operator deliberately cancels the campaign, remove its registration with:

```text
tally campaign disarm OWNER/REPO PATH/TO/WORKLIST.json
```

Do not use disarm as failure recovery.
