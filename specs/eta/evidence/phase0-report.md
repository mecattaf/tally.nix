# eta Phase 0 — bootstrap report

Written 2026-08-15 by the Phase-0 operator session (Claude Fable), executing
ETA.md §2 exactly: salvage ext0's nine lanes onto `main`, prove the assembled
tree through one bootstrap gate, deploy it as the running tally, stop.
No campaign armed, no metered (qwen/pi) tokens spent, codex untouched.

**Outcome: Phase 0 complete. PIN `72362ee520cf4e5f9d673c223a03e7ef0ba9997a`
is `main`, gate-proven and deployed. Exit state verified quiescent.**

---

## 1. Starting-state verification

Verified against the handoff's expected state at 13:1x CEST:

| item | expected | observed |
|---|---|---|
| `main` | `1384ea38`, pushed | **deviation:** `fb63a69e`, pushed — one commit ahead; the extra commit is the record commit of `main-thread-ext0-close.md` itself, made after the handoff was drafted. Judged benign, proceeded. |
| untracked files | ETA.md, AUG15-SESSION-FINDINGS.md, specs/substrate/, zeta-learnings 12/13 + raw tandem pair | exact match |
| campaign | ext0 `needs-attention`, reg `01a001a3-2c64…`, 9 done / 1 blocked | exact match (`01a001a3-2c64-7e22-bd50-47da15fa9c1f`, armSerial 9) |
| integration | `3c9600c3` | exists, tip = `authoring-doctrine-skills` |
| timers | both stopped | both `inactive` |
| daemon | active, 24 GiB drop-in | active on `yga2v3qd…-tally-0.1.0`; drop-in present, contents saved before acting |
| job units | none | none |

## 2. Steps and outcomes

### Step 1 — record commit
Staged the seven record files (all `.md`; no renames needed), ran
`nix build --no-link .#checks.x86_64-linux.language-entry-policy` on the
staged tree → PASS. Committed as `18f18238`, pushed.

### Step 2 — disarm ext0
`tally campaign disarm mecattaf/tally.nix
silent-factory-worklists/epsilon-extension.json` → `disarmed: true`;
`tally campaign list` → `[]`. The nine lane commits survive as git objects;
receipts and journal untouched; integration branch untouched.

### Step 3 — salvage cherry-picks
`git log --oneline --reverse main..3c9600c3` → exactly the nine expected lane
squash commits, no extras, none missing. Cherry-picked oldest-first
(`166ce059 final-bar-executes` … `3c9600c3 authoring-doctrine-skills`) onto
`main` — **zero conflicts**, clean tree. Result: `72362ee5`.

### Step 4 — bootstrap gate (on `72362ee5`)

| rung | result | when (CEST) |
|---|---|---|
| language-entry-policy | PASS | 13:21 |
| `cargo test --workspace` (nix develop) | PASS | 13:26 |
| `cargo clippy` | PASS | 13:27 |
| `test/fleet-gate.sh` attempt 1 | **FAIL** 14:06 — one final-bar case | |
| `test/fleet-gate.sh` attempt 2 | **PASS** 14:25 (embedded bar 24/24) | |
| `test/final-bar/run "$PWD"` | **PASS** 14:35, 24/24 | |

The attempt-1 failure, in full (the only failure of the run):

    launch-cwd-ordinary-completion (#440)  FAIL 0.22s
    same-cwd continuation failed: tally: RPC error InvalidParams:
    job 01a00549-a16f-71b3-8570-97b7c7363514 has no scraped session reference

Diagnosis (no fix applied, per the runbook): the case is **nondeterministic**.
An isolated rerun of only that case against the identical tree passed (62s
vs the 0.22s fail — the continue raced the sessionRef install), and the full
second fleet-gate attempt passed with all 24 bar cases green. Recorded as a
flake finding for eta (candidate for a Chapter 1/3 hardening note); the
witnessed green is fleet-gate attempt 2 + the standalone final bar.

### Step 5 — push
`main` = origin/main = **PIN `72362ee520cf4e5f9d673c223a03e7ef0ba9997a`**.

### Step 6 — pin + flash
Dotfiles (`~/mecattaf/dotfiles`, input `github:mecattaf/tally.nix`, pin lives
in flake.lock): `nix flake lock --update-input tally` → lock rev = PIN.
Committed **and pushed** as `9e8ce79a`, including the previously uncommitted
tracked working-tree state the running generation was built from (tripwire
journal sensor, gitAi block removal, worker-loan doc) — closing the §3.6
declared≠running trap. Untracked drafts left alone.
Tom ran `sudo nixos-rebuild switch --flake /home/tom/mecattaf/dotfiles#coordinator`
→ new system `/nix/store/jssm1fpy…-nixos-system-coordinator-26.11.20260723.e2587ca`.

### Step 7 — post-flash verification

- **7a daemon on new store path:** after remediation (see deviations),
  active on `/nix/store/gsvfv5mwizzs424cb2nixlr8pass2dp6-tally-0.1.0`
  (was `yga2v3qd…` pre-flash).
- **7b drop-in:** recreated for the new store path, 24 GiB
  (`--memory-max-bytes 25769803776`) verified live in the unit's ExecStart
  after daemon-reload + restart. The deployed pin still hardcodes 8 GiB;
  the drop-in remains the bridge until S1 deploys. **The drop-in's ExecStart
  must be re-pointed at the deployed store path after every rebuild until
  S1** — this report's §3(c) explains why.
- **7c adapter smoke:** `tally adapter smoke claude-code --assert-commit
  --pool campaign-agent` → `verdict: pass`, `captureStatus: verified`,
  `commitProbe.status: verified` (1 commit), sessionRef + finalMessage
  captured. leaseEpoch 18, witnessSeq 5353.
- **7d quiescence:** both timers `inactive`; zero `tally-job-*` units;
  `tally campaign list` → `[]`.

## 3. Deviations (all resolved, none improvised into product code)

a. **Starting `main` one record commit ahead** of the handoff's stated sha
   (§1). Benign; documented.

b. **Flaky final-bar case** `launch-cwd-ordinary-completion` (§2 step 4).
   One retry of the rung, witnessed green twice after; no code touched.

c. **The flash re-armed the declared automation.** The rebuild re-started
   both timers (they are declared-enabled), and
   `tally-producer-nightly-fleet-deploy.timer` fired during activation
   (15:04:28), enqueuing a producer job that started the system
   `fleet-deploy.service` (building the candidate from dotfiles@`9e8ce79a`
   with fresh-input override bumps). Response: both timers stopped again;
   Tom stopped `fleet-deploy.service` mid-`nix build` (15:09:36, no
   activation had occurred; unit ends `failed` by construction, and its
   OnFailure failure-surfacing hook fired — cosmetic). The producer job
   unit drained with it.

d. **The 24 GiB drop-in pins a full ExecStart, including the store path.**
   After the flash the daemon was still running the *old* binary because
   the surviving drop-in's ExecStart pointed at the old store path. Fixed
   by rewriting the drop-in against `gsvfv5mw…` + daemon restart. This is
   a standing per-rebuild step until S1 (ETA.md §5 step 4 already mandates
   re-verification at every pin-bump; the re-point is part of it).

## 4. What Tom must know

1. **fleet-deploy.service sits in `failed` state** (deliberately stopped
   mid-build; OnFailure marker fired). Harmless, but `systemctl reset-failed
   fleet-deploy.service` clears the state if the failure surfacing nags.
2. **Every future rebuild re-arms the timers and can fire the nightly
   producer immediately** if activation crosses its schedule check. Until a
   quiescence guard lands (close report §3.6 remedy, eta scope), every
   pin-bump must be followed by stopping both timers — this is now
   witnessed behavior, not speculation.
3. **The drop-in must track the store path** at every rebuild until S1
   deploys (deviation d).
4. The dotfiles push (`9e8ce79a`) means `github:mecattaf/dotfiles/main` now
   matches the running fleet — the §3.6 trap is closed, and a future
   fleet-deploy would rebuild the same declared candidate (modulo its
   fresh-input override bumps, which remain unreviewed policy).
5. The qwen window is untouched; codex untouched (quota resets Aug 20
   05:29); the only model use was the claude-code adapter smoke.

## 5. Exit state (ETA.md §2, verbatim check)

- `main` = ext0's machinery assembled and gate-proven: **yes** (PIN, gate
  table above).
- deployed pin = that rev: **yes** (daemon on `gsvfv5mw…`, built from
  dotfiles lock at PIN).
- both timers stopped: **yes**.
- no campaign armed: **yes** (`[]`).
- qwen window untouched: **yes**.

Phase 0 is closed. The orchestrator session takes it from here (eta sitting
0, worklist authoring per ETA.md §3, arm under E1–E8).
