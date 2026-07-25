# Wave 11.5 — remove `r2` from the project and its docs

**Type.** Documentation-and-scope correction. No runtime behavior changes.
**Starts from.** The Wave-11 commit (`BS-11: producers registry`). Resolve it with
`git log -1 --oneline` and confirm the tree is clean before starting.
**Ends at.** One commit. Do not begin Wave 12.

---

## 1. Why this wave exists

`r2` was specified as a producer kind — a Cloudflare R2 object-intake scanner that would
poll a bucket and enqueue OCR jobs (`docs/NIX-SPEC.md:408`, `docs/SPEC.md:402`).

It is not wanted, now or ever. R2 draining is handled outside tally by a separate Python
script, which drops event files into `events/`. The `events-dir` producer already accepts
those through the identical `validateEnqueueParams` narrower as every other kind. An `r2`
kind would therefore duplicate `events-dir` while dragging bucket credentials, pagination
state, and an S3 SDK into a process whose job is resource arbitration.

The build scope currently classifies `r2` as **deferred** (`docs/CODEX-HANDOFF.md:91`,
`docs/PRE-BUILD-ADDENDUM.md:167-168`), which implies "build it later." This wave
reclassifies it as **permanently out of scope**, and strips it from the specs so no later
wave transcribes it back in.

### The concrete risk being closed

BS-12 (the next wave) mandates that *every* option in `NIX-SPEC.md` §1–§10 be typed with a
default and an example. §3 is `producers.<name>`, and `NIX-SPEC.md:364` reads:

```
| `kind` | `enum [ "calendar" "build-effect" "pool-reachability" "gh" "events-dir" "r2" ]` | required | Discriminator. |
```

A Wave-12 agent implementing that table faithfully has a plain instruction to put `"r2"`
into the Nix option enum. It would do so without hesitation, because it is transcribing a
spec table, not making a scope judgment. That is exactly the placeholder-for-a-deferred-
thing that the handoff's deferred-not-stubbed rule forbids. Fix the table before Wave 12
reads it.

---

## 2. The one exception this wave is granted

Every prior wave carried the instruction **"keep `docs/` frozen."** Wave 11.5 is the single
authorized exception: editing `docs/` **is** the deliverable. This does not reopen `docs/`
for anything else — make only the edits enumerated in §4, and no others.

Restore the freeze in the Wave-12 handoff you write at the end.

---

## 3. Hard prohibitions

Read these before touching anything.

1. **DO NOT delete or weaken the two tests that reject `r2`.** They are at
   `crates/tally-core/src/producers.rs:1608` and `crates/tally-core/src/config.rs:310`.
   "Remove r2 from the project" means remove it from the *specs*, not remove the fence that
   keeps it out of the *code*. Those tests are the only mechanical guarantee that a future
   wave cannot add an r2 producer without a gate failing. They must still pass, unchanged
   in behavior, when this wave ends. Updating an adjacent comment to say "permanently out
   of scope" rather than "deferred" is welcome; changing what is asserted is not.

2. **DO NOT touch `Cargo.lock`.** It matches a naive `grep -i r2` only because of the
   `adler2` crate (lines 6 and 511). This is a substring false positive. Any r2 search you
   run must be word-boundaried and manually reviewed — do not let a global find-and-replace
   near this file.

3. **DO NOT touch `wave-log.jsonl`.** It is untracked and unrelated; preserve it
   byte-for-byte.

4. **DO NOT add an `r2` kind, stub, placeholder, feature flag, or "reserved" enum slot
   anywhere**, in Rust or Nix, as part of "documenting" its removal.

5. **DO NOT begin Wave 12**, and do not touch the module layer, remote lease, dmem, serving
   slices, or driver/workflow scope.

---

## 4. Exact change inventory

Ten references across six files. Each is listed with its current text and the intended
result. Line numbers are from the Wave-11 commit; re-grep rather than trusting them blindly
if the file has shifted.

### 4a. `docs/SPEC.md`

**Line 393** — the kind set:
```
`kind ∈ { calendar, build-effect, pool-reachability, gh, events-dir, r2 }`
```
→ drop `, r2`, leaving five kinds.

**Line 402** — the bullet:
```
- **r2** — R2 object intake.
```
→ delete the line entirely.

**Line 404** — the peers sentence:
```
The GitHub, calendar, and R2 sources are peers feeding the one queue — sensors, not
privileged control paths.
```
→ `The GitHub and calendar sources are peers feeding the one queue — sensors, not
privileged control paths.`

Consider adding one sentence near the `events-dir` bullet recording *why* there is no
object-store kind — that external scanners deliver through `events/` and get the same
narrowing — so the absence reads as deliberate rather than as an oversight.

### 4b. `docs/NIX-SPEC.md`

**Line 364** — the discriminator row:
```
| `kind` | `enum [ "calendar" "build-effect" "pool-reachability" "gh" "events-dir" "r2" ]` | required | Discriminator. |
```
→ remove `"r2"` from the enum. **This is the single most important edit in the wave** — it
is the one Wave 12 will read directly into a Nix type.

**Lines 408-409** — the submodule description:
```
- **`r2`** (scanner; enqueues): the R2 scanner. It carries an `enqueue` template for
  the OCR jobs it emits.
```
→ delete both lines.

### 4c. `docs/BUILD-SEQUENCE.md`

**Line 318** — BS-11's scope sentence:
```
sources), `r2`, `build-effect` (store-path single-flight dedup), and
```
→ remove `` `r2`, ``. Note this line is already historically stale (BS-11 shipped without
r2); the edit aligns the record with what was actually built.

### 4d. `docs/COVERAGE-MATRIX.md`

**Line 68** — the port row:
```
| events-dir/drain/r2/gh sensors | PORT + unify into the producers kind registry | producers |
```
→ `| events-dir/drain/gh sensors | PORT + unify into the producers kind registry | producers |`

### 4e. `docs/CODEX-HANDOFF.md` *(untracked — see §5)*

**Line 91** — the deferred list:
```
re-adoption, the r2 producer, full BS-13 golden-diff harness, the rest of BS-14.
```
→ remove `the r2 producer, ` from the **deferred** list, and add r2 to the **NEVER** list
in the following sentence, with its own rationale. r2 does not belong under that list's
existing "driver-layer by the one law" justification — it is out for a different reason
(subsumed by `events-dir`), so give it a distinct clause rather than silently filing it
under the wrong rationale.

### 4f. `docs/PRE-BUILD-ADDENDUM.md` *(untracked — see §5)*

**Lines 167-168** — F1's deferred ruling:
```
and cross-host re-adoption, r2 + calendar… correction: **calendar stays** (daily-steering
pacing) — deferred producers are r2 only; BS-13 golden-diff beyond the BS-1 dominant test;
```
→ the clause "deferred producers are r2 only" must become "there are no deferred
producers." Keep the calendar correction intact — calendar stays, and that ruling is
unrelated.

Then record the reclassification. F2 is "Standing OUTs restated", but its framing is
"these live in the Agency driver," which does not describe r2. Add r2 as a separate
standing-OUT entry (an **F3**, or a clearly-marked clause within F2) whose stated reason is
that `events-dir` already subsumes external object-store intake. Do not distort F2's
existing rationale to absorb it.

---

## 5. The untracked-docs question — resolve it explicitly

`docs/CODEX-HANDOFF.md` and `docs/PRE-BUILD-ADDENDUM.md` are **untracked**, and prior waves
carried an instruction to preserve the three untracked files byte-for-byte.

That instruction is **superseded for these two files only, for this wave only.** They are
the documents that define r2 as deferred, so leaving them untouched would defeat the
purpose — a future agent reading `CODEX-HANDOFF.md:91` would still see r2 listed as
deferred work awaiting implementation.

`wave-log.jsonl` remains untouched and preserved.

Because these two files are untracked, your edits to them will **not** be captured by the
commit. This is expected. You must therefore state plainly in `WAVES-STATE.md` that both
were modified in-place and what changed, so a later wave does not read the divergence as
unexplained drift. If the user would prefer them tracked, raise it rather than deciding
unilaterally — `git add` on a file three waves have deliberately left untracked is not
yours to choose.

---

## 6. Gates

This wave changes no runtime behavior, so the heavy ritual does not apply. **No live
hardware run is required** — there is nothing new to exercise on the worker, and Wave 11's
live evidence still covers the code as committed.

Run, from `nix develop`:

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace` — the r2-rejection tests must still be present and passing
- `nix flake check -L`

Then the completeness check — this must come back empty except for `Cargo.lock`:

```
git grep -n -i -w 'r2' -- . ':!Cargo.lock'
grep -rn -i -w 'r2' docs/
```

If either returns a hit you did not consciously decide to keep, the wave is not done.
Search case-insensitively: the specs use both `r2` and `R2`.
