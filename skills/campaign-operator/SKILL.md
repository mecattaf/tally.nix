---
name: campaign-operator
description: Operate an ad-hoc forge-native tally campaign as its supervisor — arm it, watch it, steer it, and survive its failures — encoding every operator mistake the first real campaign (crm-call-drain, 2026-08-07, tally.nix#430) paid for. Use when arming or supervising a tally campaign, re-arming after a failure, or diagnosing a campaign that died before dispatch. Companion to assign-tally (which owns the prepare/launch/observe contract); this skill owns the human at the controls. WIP — lives in tally.nix until stable, then graduates to dotfiles.
---

# Operate a campaign without becoming its second defect

The 2026-08-07 crm-call-drain run reached zero agent dispatches in a full day of
supervision — every intervention was mechanism-level, and several were operator
error. This skill encodes that ledger (tally.nix#430) so the next campaign spends
its failures on new problems.

## Rule of residue

Same contract as assign-tally: this skill may only contain rules the mechanism
cannot yet enforce. Every rule carries a `DEBT:` marker naming the mechanism change
that should absorb it; when that change ships, delete the rule here. A growing
skill is a failing mechanism.

## 1. The master body freezes at projection. Prose goes in comments.

`project` → verify → `arm` → from that moment every human word is a **comment**
(master issue = campaign-wide, sub-issue = per-task steering). Comments never touch
the digest; body edits always rotate revisions and force a re-arm — and even when a
digest failure is really something else (#429 was), a post-projection body edit
poisons the diagnosis: two of the four burned re-arms on 2026-08-07 chased that
edit instead of the real defect.
DEBT: #433 (a receipt that names the divergent path makes a body-edit mismatch
self-identifying); until then the rule is absolute.

## 2. Probe the mechanism before the mission.

The first ad-hoc arm on a host or pin is always a **probe campaign**: one task,
shell adapter, trivial gate, ten minutes. It would have caught #429 and the
projection fragility with zero noise in the real issue set. The freeze ritual's
adapter smokes and gate argv runs are necessary, not sufficient — they do not
exercise reconcile, and reconcile is where the first real campaign died four times.
DEBT: campaign preflight verb (#248) growing a one-task self-test mode.

## 3. Checkpoint argv is single-line and control-char-free.

`campaign project` correctly rejects newlines at validation, but nothing says so
where checkpoints are introduced, and a heredoc-bearing script bounces after you
have written it. The pattern that works: `sh -euc '<one line; python3 -c for real
logic>'` — mind backtick and quote hazards inside the one line.
DEBT: campaigns doc states the single-line contract where checkpoints are
introduced; delete this rule when it does.

## 4. pi as campaign agent — the recipe.

Stock pi presets declare `launch = {}`: no model override, no policy maps. For a
pi/qwen campaign agent: set `approvalPolicy` and `sandboxPolicy` to null, pin the
model in pi's own `settings.json` (defaultProvider/defaultModel), and know the
ad-hoc agent schema's field set exactly (#429: the CLI and driver disagreed inside
one pin; check `campaign project` accepts your manifest before arming). Smoke with
`tally adapter smoke pi --pool campaign-agent --assert-commit` — pi has no
conventional lane, so `--pool` is required, and `campaign-agent` is the
campaign-relevant choice. A pi node must be resumed in the cwd it was launched in
(tally.nix#425).
DEBT: #429 (schema parity in CI) and #425 (enforced resume invariant) shrink this
to the null-policy + model-pin recipe.

## 5. Daemon-side truth outranks CLI results during stalls.

While the daemon is congested, RPC reads time out and `adapter smoke` can report
failure for work whose daemon-side verdict was witness-emitted PASS. Before acting
on any CLI-reported failure, corroborate ground truth: witness records, capture
`.err` files (their *presence* is a failure signal; their absence plus
`capture: <not retained>` means nothing to read), merged PRs, runner-unit
liveness. The journal filter that isolates campaign signal: campaign pools +
evidence_fail/diagnosis/escalation, minus RPC noise.
DEBT: #434 (three-valued smoke verdicts, `query run` durable fallback) absorbs the
corroboration ritual; keep the filter recipe until journal events carry a
campaign-scoped key on early flow nodes (#430 finding 5, residue).

## 6. Check estate load before arming.

"Is the daemon quiet enough for the projection window" is a preflight gate. One
`ls ~/.local/state/tally/events | wc -l` and one journal grep for dispatch-loop
absence lines cost less than one dead pass. A saturated daemon (30k durable rows,
60–183 s stalls on 2026-08-07) kills campaign passes probabilistically at every
driver node; nightly-window scheduling is the honest default on a loaded estate.
DEBT: #431 (the stall fix) and #432 (retryable-projection) retire this as a
hard gate; it then becomes ordinary capacity hygiene.

## 7. The packaged halves are one pin, and overrides are debts.

The arm CLI and the packaged drivers version together; a defect can sit between
them (#429). `arm --driver` / `--flow` overrides are sanctioned escape hatches —
but every local override is **disclosed** (an issue plus a campaign-thread comment
naming the exact change) and **deleted on pin advance**. An undisclosed override
is a fork of the mechanism wearing its name.

## Failure economics (what the first run's numbers say)

Four re-arms were burned on plausible-but-wrong theories before source-diving the
canonicalizations; two smoke false-negatives cost real diagnosis time; the one
probe campaign that was never run would have cost ten minutes. When a campaign
fails pre-dispatch: read the receipt evidence first, corroborate daemon-side truth
second, theorize last — and if you are about to re-arm on a theory, ask what the
probe-campaign version of that theory costs.
