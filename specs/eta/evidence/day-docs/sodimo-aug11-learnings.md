# sodimo-aug11-learnings.md — tally findings from the sodimo-os program
# (chapters 1–3, 2026-08-11 15:17Z → 2026-08-12 07:57Z, pin 23509ab)

Context: three consecutive forge-native ad-hoc campaigns in `sodimo/os`
(private fork of cloudflare/cloudflare-os), serial codex lanes,
danger-full-access, brief-from-TALLY_BRIEF, hardened-tier witnessed gates
(pnpm/TypeScript monorepo — first campaigns on this workload class).
**Result: 33/33 tasks settled, 30 PRs merged, 3 checkpoints green, zero
failed attempts, zero re-triggers, zero steering comments, zero human
interventions.** Build state at stop: `~/leger/sodimo-os-campaign/STOP-STATE.md`.

Findings ordered by weight. F-01/F-02 are the ones worth code changes.

## F-01 (was L-04) — `tally query run <arm-uuid>` silently goes stale after
## the arm→poller handover; there is no live campaign-status verb

The biggest operational trap. `campaign arm` admits only the FIRST flow run;
after it ends, `tally-campaign-poll` reconciles, and each cycle with work
spawns a NEW flow run (fresh task_uuid) that re-walks the graph from forge
facts. Querying the arm-time uuid forever replays its final snapshot. In ch1
this had the supervisor watching "1 done, 1 pending, 7 blocked" for THREE
HOURS while tasks 2–8 merged every ~25 min and the campaign completed and
closed its own master. Nothing in the query output hints it is superseded.

Asks:
1. `tally query run` on a finished flow run that has campaign descendants
   should print a "superseded — campaign advanced; latest flow run <uuid>"
   pointer (barrier/parent linkage exists to derive it).
2. Better: a `tally campaign status <master-issue-url>` verb resolving the
   live view (registration → latest observation → per-task forge state).
3. Doc line in the campaigns doc: "the arm-time run is not the campaign".

Supervision doctrine that worked for ch2/ch3 (10+11 tasks, zero surprises):
watch the forge (`gh issue list`/`gh pr list -R <repo>`, master comments,
`tally:campaign-complete` marker) + the poll journal
(`{observed,dispatched,pruned,blocked,failures}`); `pruned:1` after a
campaign-complete comment is normal cleanup.

## F-02 (was L-02) — campaign gate preflights run as an arm-time sweep in a
## PRISTINE worktree; sequencing preflights are a landmine, and they fail silently

Ch1's first arm died in seconds: gate `lint` preflight `test -d
node_modules` (authored as "install ran before me") can never pass in the
pristine admission sweep. Tally behaved well — failed fast, zero lanes, exact
gateId/taskRef — but TWO rough edges:
1. `sh -euc 'test …'` fails with EMPTY stderrExcerpt and a 0-byte .err
   capture; the error should include the failing preflightArgv verbatim
   (tally has it in hand).
2. No lint heuristic (cf. #484's argv linter) or schema-doc warning exists
   for "preflight depends on gate-produced state".

Practices adopted (recommend as doctrine): preflights assert ENVIRONMENT
only (tools on PATH, tracked files readable); every preflight check echoes a
diagnostic (`|| { echo "preflight: …" >&2; exit 1; }`); the pre-arm freeze
rehearsal must run preflightArgvs in a pristine worktree, not just gate
argvs. Recovery path (fix worklist → `project --issue` idempotent, same
sub-issue numbers → re-arm, new payloadHash supersedes) worked first try.

## F-03 (was L-01) — #484 arm-time argv linter false-positives on in-argv
## mitigations (11 warnings per arm, 33 total, all benign)

- "/tmp path; PrivateTmp hides staged paths" fires even when the argv itself
  `mkdir -p`s the path inside the unit. Suppress when the argv creates the
  path before use.
- "invokes nix without cache/state redirect" pattern-matches the substring
  `nix` in `command -v nix` preflights that never evaluate. Only flag nix
  followed by an evaluating subcommand (develop/build/shell/run).
Every arm emits the same 11 lines; benign-but-noisy warnings train operators
to skim warnings, which is how real ones get missed.

## F-04 (was L-05) — poll JSON counters are fleet-wide, not per-campaign

Mid-ch2 the poll reported `observed:2 … pruned:1`; the pruned registration
was an UNRELATED campaign (tally.nix#527 flow-render-467) completing.
A supervisor must resolve counters against
`~/.local/state/tally/campaigns/armed/*.json` (registrations carry issueUrl)
before reacting. Positive finding: two concurrent campaigns coexisted on one
host (shared pools) with zero interference — ch2's erp-mirror merged during
the overlap. Ask: include the registration/issue identifier in per-campaign
poll events, or emit one JSON line per registration.

## F-05 (was L-06) — nightly fleet-deploy has an exec-condition guard

2026-08-12 02:00, with ch2 live: "Skipped due to 'exec-condition'" — the
deploy never ran, daemon uptime unbroken. The leave-the-timer-alone doctrine
is therefore doubly safe (pin-stability analysis AND a runtime guard). Ask:
document next to the timer what the condition checks, so future operators
don't re-derive it under pressure.

## F-06 (was L-03) — supervisor gotcha: bare `gh` resolves to the `upstream`
## remote in fork checkouts

In a checkout with origin=sodimo/os + upstream=cloudflare/cloudflare-os,
`gh pr view 11` silently queried upstream ("PR not found" / wrong issue).
Tally is unaffected (pins the repo); all supervision tooling must pass
`-R <owner>/<repo>`. Worth one line in the campaign-operator skill.

## F-07 — post-completion reconcile noise (cosmetic but misleading)

The final poll cycles before a completed campaign's prune emit
`FlowResultError result-schema-mismatch` + a 10 s finalMessage projection
timeout wrapping the true, correct message ("campaign master issue must be
open and canonical"). It reads as a failure at the exact moment of success
and buries the state. A closed-canonical master should short-circuit to a
clean "complete → pruning" event. (Also seen: a one-cycle `rearm-required`
digest flap while tally itself was mutating issues — self-inflicted forge
writes shouldn't trip the rearm check even transiently.)

## F-08 — per-flow usage accounting fragments across the poller's flow runs

`tally query run` usage lines only cover that flow run's attempts; a
campaign's true token spend is scattered across arm + N poller runs, with
"attempts-without-usage" partials for adapter-advisory captures. A
campaign-level usage rollup (natural home: the proposed `campaign status`
verb, or the campaign-complete comment) would make cost visible where
decisions are made.

## Positive findings — what carried the program (keep these behaviors)

1. **The Aug-10/11 hardening held completely**: no steward grammar issues,
   no pardon problems, no steering races, no marker-walk taxes, no session
   deaths, no gate deaths — across 33 tasks on a NEW workload class
   (pnpm/TS monorepo) and a brand-new private repo/org.
2. **Witnessed-gates-only merging needed zero human judgment**: 30 PRs
   merged on green ladders; not one required opinion.
3. **The codex survival preamble works**: lane commits per milestone, no
   shell `rm -rf`, inspect-preserved-state-before-redo — zero lane losses.
4. **Adversarial suite tasks are the highest-value brief pattern**: the two
   dedicated adversarial lanes (ch2 scope-suite, ch3 mail-authz-suite)
   found and fixed FIVE real security holes the implementation lanes'
   own tests missed (stale-principal access post-suspension; foreign-parent
   linking; SMTP AUTH credential leak via debug/error paths;
   existence-leaking denial semantics on foreign mailboxes; delegation
   grants accepting non-admins) — each with regression tests, all inside
   the same witnessed-gate discipline. Recommend: every campaign ships one
   end-of-chapter adversarial task with a fix-in-place mandate and
   structural (not hand-listed) verb enumeration.
5. **Serial linear chains beat clever DAGs for unattended runs**: chaining
   migration-writing and chokepoint-file tasks eliminated the whole
   renumber/conflict class; cost nothing at maxParallel 1.
6. **Idempotent `project --issue` is the recovery workhorse**: used three
   times (GitHub 502 mid-projection, preflight fix, pre-arm brief edit),
   always converging on the same sub-issue numbers.
7. **Throughput for planning**: 33 tasks in ~16.7 h wall clock including
   authoring gaps; steady-state ~28–32 min/task (codex session dominates;
   gate ladder ~2 min hot). Ch3's 12 tasks ran 01:20→07:57 unattended.

## Program stats

| Chapter | Tasks | PRs | Wall clock | Holes found+fixed |
|---|---|---|---|---|
| 1 (skeleton) | 9 | #11–#18 | 3h01 | — |
| 2 (CRM/data/perf) | 12 | #32–#42 | 5h55 | 2 |
| 3 (mail/skills/AI) | 12 | #56–#66 | ~6h37 | 3 |

Masters: sodimo/os #1, #19, #43 (all closed by tally with campaign-complete
markers). Checkpoint trees: b957224, 8244cd6, 092c105. Estate at stop:
no armed registrations, no running job units, daemon idle — safe for
tally.nix surgery.
