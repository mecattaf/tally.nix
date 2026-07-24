# Dotfiles prior art for tally-flow

Normative input for the tally-flow spec. Sourced from
`/home/tom/mecattaf/dotfiles` (read locally, not cloned) at the working tree
present 2026-07-24. Every quote below is verbatim from the cited file; do not
paraphrase these into the spec without re-checking against source if the
dotfiles tree has moved on.

Source files read:

- `docs/local-ai/pi-appliance-pattern.md`
- `docs/local-ai/model-roster.md`
- `docs/local-ai/README.md`
- `docs/local-ai/monthly-workflow.md`
- `pkgs/local-ai-monthly/default.nix`
- `pkgs/local-ai-monthly/supervisor.sh`
- `pkgs/local-ai-monthly/stages.nix`
- `pkgs/local-ai-monthly/lib/judge.sh`
- `lib/local-models.nix`
- `lib/local-model-backends.nix`
- `home/tally.nix`

---

## 1. Pool selector grammar, verbatim

Source: `docs/local-ai/pi-appliance-pattern.md`, section "Abstract model
selectors" (lines 32–50).

> Workflow procedure names a capability class, never a checkpoint. Current
> single-member classes are `strongest` and `fast`, resolved against the
> accepted catalog and the models currently advertised by llama-swap.
>
> A pool selector extends that vocabulary without changing the task contract:
>
> ```text
> pooled-fast(4, diversity=base-family)
> pooled-strongest(3, diversity=maker)
> ```
>
> `count` is the required number of independent members. `diversity` prevents
> a pool from becoming several nearly identical quants or fine-tunes when the
> goal is broader hypothesis coverage. Useful keys include base checkpoint
> family, maker/frontier lab, architecture, fine-tune lineage, backend, and
> modality. Resolution is deterministic and its concrete member list is
> written to the witness before inference begins.

Reading of the grammar:

- Base (non-pooled) selectors are bare capability-class names: `strongest`,
  `fast`. These resolve to exactly one member.
- Pool selectors wrap a base class name as a function-call-shaped form:
  `pooled-<class>(<count>[, diversity=<key>])`.
- `count` (first positional arg) is the **required** number of independent
  members — this is `required_members` for the pool's quorum (see §2), not
  merely a hint.
- `diversity=<key>` is optional and named. Valid keys named in the doc: base
  checkpoint family, maker/frontier lab, architecture, fine-tune lineage,
  backend, and modality. The doc does not give an exhaustive enum — it says
  "useful keys include," so the schema should treat this as an open string
  key resolved against roster fields, not a closed enum, unless the spec
  chooses to close it.
- Only two capability classes are named as currently existing: `strongest`
  and `fast`. The spec should not invent additional classes (e.g. no
  `pooled-coding` in the source) — classes are resolved "against the accepted
  catalog and the models currently advertised by llama-swap," i.e. resolution
  is a join between a static roster and a live llama-swap `/v1/models`
  response, not roster alone.
- **Hard requirement, load-bearing for witness design**: "Resolution is
  deterministic and its concrete member list is written to the witness
  *before* inference begins." This means the flow host must persist the
  resolved member list (concrete model IDs) to durable state as a discrete
  step prior to launching any pool member process — this is not something
  that can be inferred retroactively from what ran.

---

## 2. Map/validate/reduce contract

Source: `docs/local-ai/pi-appliance-pattern.md`, sections "One atomic
member," "Pool: map, validate, reduce," "Swarm," and "Quorum, repair, and
failure" (lines 13–31, 52–119).

### Fresh process, immutable shared evidence, no cross-visibility

> Every model task has five versioned parts:
>
> 1. an immutable, bounded input bundle prepared by ordinary code;
> 2. one self-contained Pi skill describing the judgment procedure;
> 3. a selective list of task-specific tools with hard quotas;
> 4. a structured result schema plus deterministic semantic validation;
> 5. one repair attempt in a fresh Pi process when validation fails.
>
> Pi is the agent harness. A workflow executable may invoke Pi repeatedly, but
> it must not recreate Pi's agent loop, provider layer, sessions, or tool
> protocol. Built-in general-purpose tools are disabled when narrow tools
> suffice.
>
> The trace for a member records at least the task ID, concrete provider/model
> ID, skill and workflow revision, input digest, tool-call outcome, output
> digest, validation errors, repair count, and terminal status. Raw context
> may remain in the runtime directory; the compact lineage belongs in Tally's
> local witness.

Pool diagram and independence rule:

> ```text
> immutable task bundle
>   ├─ Pi member A ─ validate ─ candidate A
>   ├─ Pi member B ─ validate ─ candidate B
>   ├─ Pi member C ─ validate ─ candidate C
>   └─ Pi member D ─ validate ─ candidate D
>                          ↓
>                typed reducer input
>                          ↓
>                  one final result
> ```
>
> Each member gets a fresh Pi process, the same immutable evidence, the same
> skill revision, the same narrow tools, and no other member's answer. This
> preserves independence. Each candidate must pass the task schema and
> provenance rules before it can enter reduction.

Load-bearing details: **fresh process per member** (not a shared long-lived
agent fanning out internally); **same immutable evidence** bundle to every
member (no per-member evidence drift); **no other member's answer** visible
to any member (independence — rules out any "see other drafts" pattern); and
schema+provenance validation gates entry to the reducer — an invalid
candidate cannot silently participate.

### The three reducer classes, exact semantics

> The reducer is explicit:
>
> - `identity` for one member;
> - `deterministic` for union, keyed deduplication, voting over exact enums,
>   numeric aggregation, or other semantics ordinary code can own;
> - `pi-aggregate(<class>)` when synthesis itself requires judgment. The
>   aggregator receives only the original task identity, validated candidate
>   outputs, their model IDs/digests, and stable evidence handles. It does not
>   receive hidden transcripts and may not invent evidence.
>
> An aggregator must preserve dissent. It records which candidates support
> each conclusion, which conflict, and which were excluded by validation.
> "Majority" is not evidence and does not erase a minority result backed by
> stronger primary provenance.

Reading:

- `identity` — degenerate case, single member, no real reduction; used when
  `count == 1` (i.e. plain `strongest`/`fast`, not a pool).
- `deterministic` — pure code, no model call. The doc enumerates union,
  keyed dedup, voting over exact enums, numeric aggregation as example
  semantics "ordinary code can own." This reducer class must be fully
  specified by the flow author (it is not a magic catch-all) — implication
  for the flow dialect: `deterministic` reducers need a sub-selector or
  inline function naming which of these operations to run.
- `pi-aggregate(<class>)` — itself a *further* model call, parameterized by a
  capability class (presumably `strongest`/`fast`/pooled again, though the
  doc doesn't nest pools inside aggregators explicitly). Its input contract is
  narrow and explicit: task identity + validated candidates + their
  model IDs/digests + stable evidence handles — explicitly **not** hidden
  transcripts, and the aggregator "may not invent evidence." This is a hard
  constraint on what data the flow host is allowed to pass into an aggregate
  reduction step.
- **Dissent preservation is mandatory for `pi-aggregate`**: the reducer output
  must record, per conclusion, which candidates support it, which conflict,
  and which were excluded by validation. The doc explicitly rejects majority
  vote as sufficient ground for erasing a minority result "backed by stronger
  primary provenance" — i.e. the reducer's output schema needs
  support/conflict/excluded attribution fields, not just a winning answer.

### Swarm (typed DAG) — quoted in full in §5 below; contract note

> Stages communicate only through validated artifacts. A parent does not
> expose a child's specialist tools to itself, and a child does not inherit
> general shell or network access. This is procedural fan-out/fan-in, not an
> unconstrained chat between agents.

This generalizes the map/validate/reduce independence rule to multi-stage
DAGs: no tool inheritance across stage boundaries, artifact-only
communication.

### Quorum fields, one-repair-attempt, fail-closed

> Every fan-out declares `required_members`, `minimum_valid`, and whether
> partial reduction is allowed. A missing or invalid member is visible in the
> reducer input and witness. It is never silently replaced by duplicated
> output from a surviving model.
>
> Each member receives at most one clean contract-repair attempt. Reducers
> follow the same rule. If quorum is not met, the stage fails closed and
> durable state does not advance. A task-specific workflow may still render a
> degraded human briefing, but it must say which members or stages failed.

Exact field list a flow's fan-out declaration must carry:

- `required_members` — the target member count (== the pool selector's
  `count`).
- `minimum_valid` — the floor of *valid* (schema+provenance-passed) members
  below which the stage cannot proceed to reduction.
- a partial-reduction flag/policy — whether reduction may run when
  `minimum_valid <= valid_count < required_members`.

Failure handling:

- Missing/invalid members must appear explicitly in both the reducer's input
  and the witness — never masked by silently duplicating a surviving member's
  output to pad the count.
- **One repair attempt** applies uniformly to members *and* reducers — a
  single fresh-process retry on contract violation, not a retry loop.
- If quorum (`minimum_valid`) is not met after the repair attempt, the stage
  **fails closed**: durable state does not advance. The workflow is permitted
  to emit a degraded human-facing briefing but must explicitly name which
  members/stages failed — it cannot present a falsely-complete result.

---

## 3. The roster contract

Source: `lib/local-models.nix` (NixOS module-system schema, lines 1–314) and
`docs/local-ai/model-roster.md` (human view), plus operating rules
(`model-roster.md` lines 54–64).

### Structure enforced by `lib/local-models.nix`

Two top-level attrsets, each a `types.attrsOf` of a submodule: `artifacts`
(physical files/weights) and `deployments` (roster rows exposed through
llama-swap). This split matters: an artifact is not itself a catalog entry: a
`deployment` references one or more `artifacts` by key (`artifacts.model`,
`artifacts.mtpHead`, `artifacts.mmproj`, `artifacts.tokenizer`,
`artifacts.template` — see `artifactRefsType`, lines 147–170).

**`artifactType`** fields (lines 98–145):

- `kind`: enum `["model" "mtp-head" "mmproj" "tokenizer" "template"]` — an
  artifact's role in a deployment.
- `maker`: str — org/person that trained the artifact.
- `baseCheckpoint`: nullable `{ url; revision; }` — the base model this
  artifact derives from.
- `fineTune`: nullable `{ url; revision; }` — the fine-tune lineage, if any.
- `source`: `{ hfUrl; revision; primary; files: [{ path; bytes; oid; hash; }]; }`
  — exact pinned HF revision, primary file path (first part for split
  GGUFs), and one-or-more file entries each carrying exact byte size, LFS
  `oid`, and Nix SRI `hash`.
- `notes`: str, default `""`.

**`deploymentType`** fields (lines 232–313) — this is the catalog row a
caller actually selects against:

- `model`: str — "Model ID presented through llama-swap." This is the
  identity a flow's selector resolution must match against the live
  llama-swap `/v1/models` response.
- `role`: enum `["utility" "coding" "general" "quality" "vision" "embedding"
  "uncensored" "draft"]` — this is the **capability class** dimension a
  selector like `pooled-fast`/`pooled-strongest` presumably filters by, jointly
  with `status`.
- `status`: enum `["canonical" "candidate" "experimental" "negative"
  "retired"]` — only `canonical` rows are live-selectable in practice per the
  monthly-workflow doc's routing language, though the schema itself doesn't
  hard-code that filter.
- `backend`: enum of `backendKinds.local ++ backendKinds.peers`, where
  `lib/local-model-backends.nix` defines:
  ```nix
  {
    local = [ "rocm" "vulkan" "ds4" "vllm" "mlx" ];
    peers = [ "npu" ];
  }
  ```
- `hosts`: non-empty list of enum `["coordinator" "worker"]` — physical
  placement.
- `ramTierGb`: unsigned int, default `0`.
- `artifacts`: `artifactRefsType` — refs into the `artifacts` attrset by key
  (all nullable, default `null`).
- `runtime`: `runtimeType` = `{ repository; commit; args: [str] (default []); }`
  — exact pinned runtime source and backend argv. `args` supports
  `@model@`/`@mtpHead@`/`@mmproj@`/`@tokenizer@`/`@template@` placeholder
  substitution to immutable store paths.
- `peer`: nullable `{ name; proxy; systemdUnit: nullable; }` — for
  non-llama.cpp peers (e.g. FastFlowLM NPU) fronted by llama-swap.
- `benchmark`: nullable `{ sourceRepo; sourceCommit; runId; name; score:
  nullable; speed: nullable; context: nullable; }`.
- `evidence`: enum `["matched-local" "upstream-measured" "api-only"
  "unverified"]` — this is a **confidence/verification tier**, distinct from
  `status`. A catalog JSON schema for tally-flow should carry this as a
  separate axis a selector/diversity resolution could in principle filter or
  report on.
- `hardware`: str, default `""`.
- `supersedes` / `supersededBy`: nullable str — lineage links between rows.
  This is the closest existing analogue to a "fine-tune lineage" diversity
  key at the deployment-row level (vs. `artifactType.fineTune` at the weight
  level).
- `notes`: str, default `""`.

### Fields a catalog JSON schema for tally-flow must carry

Given the above, the minimum field set a machine-readable roster/catalog JSON
needs to support the pool selector's `diversity` keys ("base checkpoint
family, maker/frontier lab, architecture, fine-tune lineage, backend, and
modality") is:

- model ID (`model`)
- base checkpoint family/identity (`artifacts.model` → `baseCheckpoint.url`)
- maker (`artifacts.*.maker`)
- fine-tune lineage (`artifacts.*.fineTune`, and/or `supersedes`/`supersededBy`)
- backend (`backend` enum)
- modality (closest existing proxy is `role` — `vision` vs `embedding` vs
  text roles — the doc does not have an explicit modality field separate from
  role)
- role/capability class (`role`)
- status and evidence tier (`status`, `evidence`) — needed to restrict
  pool membership to live, trustworthy rows
- host placement (`hosts`) — needed to know which compute host a resolved
  member requires

`nix eval --json ... #lib.localModelCatalog` is the exact mechanism the
monthly workflow uses to materialize this Nix-side roster into JSON
(`supervisor.sh` line 202); tally-flow's host helpers should follow the same
"evaluate the Nix attrset to JSON, then join against live llama-swap
`/v1/models`" two-step rather than inventing a separate catalog format.

### Operating rule: ensemble policy lives in the caller

Source: `docs/local-ai/model-roster.md`, "Operating rules" (lines 54–64):

> - Uncensored rows are direct-address/manual only. They generate hypotheses;
>   evidence-bearing models or humans judge them.
> - The coder pool exposes individual model IDs. Ensemble policy belongs in
>   the caller and must preserve each response and attribution.
> - The 8B OCR model drains by default. The 32B model is conditional
>   refinement, not a permanently co-resident second server.
> - DeepSeek V4 is fleet-exclusive. Both nodes must drain other GPU work
>   before a DS4 session.
> - Audio, image, and video generation remain parked and have no roster
>   entries.

The load-bearing line for the flow dialect: "The coder pool exposes
individual model IDs. Ensemble policy belongs in the caller and must
preserve each response and attribution." The roster itself is deliberately
dumb — it does not hide a pool behind a synthetic combined model name, and it
is the *flow*/host layer's job to implement pooling, and to preserve
per-response attribution (this directly matches the dissent-preservation
requirement in §2).

Also relevant, `docs/local-ai/README.md` (lines 34–36):

> - **Coder/swarm:** three separately addressable models. A caller may
>   request pooled opinions, but the catalog does not hide them behind a
>   synthetic model name or silently vote on results.

And uncensored-row exclusion from automatic routing, `README.md` line 37-39:

> - **Uncensored:** three manually addressed, cross-family/refusal-removal
>   routes. They are high-recall hypothesis generators, never arbiters, and
>   must not enter automatic routing.

This is a hard constraint a `pooled-*` selector resolver must respect:
uncensored-role rows are excluded from any automatic/pool resolution
regardless of what `diversity` key is requested.

---

## 4. `supervisor.sh` mechanics

Source: `pkgs/local-ai-monthly/supervisor.sh` (full file, 343 lines),
`pkgs/local-ai-monthly/default.nix`, `pkgs/local-ai-monthly/stages.nix`,
`pkgs/local-ai-monthly/lib/judge.sh`, and `home/tally.nix` (producer
`monthly-local-ai-review`, lines 133–159).

This is the single shipped instance of "a linear workflow with substantial
non-GPU work holds its own workflow mutex and enqueues a declared, bounded
child for the GPU-only stage" (the pattern doc's own description, §6 quote
below). It is bash, not the flow dialect, but it is the exact shape tally-flow's
host helpers need to reproduce declaratively.

### Top-level flow, in order

1. **Argument parsing and validation** (lines 1–88): `--publish`,
   `--prepare-only` (mutually exclusive), `--period YYYY-MM`, `--dotfiles-url`,
   `--base-branch`, `--runtime-base`, `--state-dir`, `--tally`. Regex-validates
   `period`, `base_branch`, and (if publishing) `github_repo`. Sets
   `GIT_TERMINAL_PROMPT=0`, `LC_ALL=C`, `umask 077`.
2. **Receipt scaffolding** (lines 90–176): creates `run_dir` under
   `runtime_base` via `mktemp -d`, sets a fixed `receipt` path at
   `$state_dir/last-run.json`, installs an `on_exit` trap that always calls
   `write_receipt` (schema_version 2 JSON: period, run_dir, timestamps,
   status, error, dotfiles_commit, model_id, three Nix store output paths,
   commentary sha256, per-source summary, pr_url, and the fixed assertion
   `no_model_blobs: true`) regardless of success/failure — this is the
   pattern's durable local witness.
3. **Deterministic Git capture** (lines 178–201): shallow-blobless clone of
   `dotfiles_url` at `base_branch` into `run_dir/dotfiles`; reads
   `pkgs/local-ai-monthly/sources.json` as `$registry`; asserts
   `schema_version == 1`; invokes `$LOCAL_AI_CAPTURE` (a separate
   `writeShellApplication`, `lib/capture.sh`) to clone each watched source and
   produce `capture/manifest.json`. If `changed_count == 0` across all
   sources, status becomes `no-delta` and the run exits 0 early — no wasted
   GPU/Pi work when nothing changed.
4. **Catalog + live models join** (lines 202–212): `nix eval --json
   --no-write-lock-file "path:$dotfiles#lib.localModelCatalog" >
   capture/catalog.json`, then a bounded `curl` (10s connect / 30s max) to
   the llama-swap `/v1/models` endpoint (derived from the registry's
   `.inference.url`), asserting the response is a non-empty JSON array. This
   is the exact "resolved against the accepted catalog and the models
   currently advertised by llama-swap" join described in §1.
5. **Prior tally snapshot** (lines 214–220): copies the most recent
   `docs/local-ai/tallies/YYYY-MM*.md` file into `capture/accepted-tally.md`
   if any exist.
6. **`prepare` Nix build** (lines 222–230): `nix build --offline --no-link
   --print-out-paths --file $LOCAL_AI_STAGES --argstr phase prepare ...`
   against `stages.nix`, a `builtins.derivation` wrapper (see below) that
   invokes the pure-stage shell script sandboxed, network-free.
7. **HF metadata capture + `enrich` Nix build** (lines 232–243): a bounded
   `LOCAL_AI_HF_CAPTURE` call (byte-limited via
   `.limits.hf_metadata_response_bytes`, default 5,000,000) fetches HF
   metadata responses outside the sandbox, then a second `stages.nix` build
   (`phase enrich`) folds those exact responses into the immutable evidence
   bundle deterministically.
8. **`--prepare-only` early exit** (lines 245–249): stops here if requested,
   before any GPU/Pi work — used to validate the whole non-GPU path.
9. **The Tally child-enqueue for the GPU-only Pi stage** (lines 251–280) —
   this is the mechanism of most direct interest to tally-flow:
   ```bash
   if [[ -z "${TALLY_SOCKET:-}" || -z "${TALLY_JOB_ID:-}" ]]; then
     printf 'local-ai-monthly: the Pi step requires a parent Tally job with child enqueue capability\n' >&2
     exit 1
   fi
   "$tally_program" --socket "$TALLY_SOCKET" enqueue \
     --source orchestrator \
     --pool worker-gpu \
     --priority low \
     --dedup-key "local-ai-judge-$period-${evidence_digest:0:20}" \
     --runtime-max-sec "$model_timeout" \
     --no-enqueue \
     --wait \
     --evidence exit:0 \
     --evidence "artifact:$commentary" \
     --evidence hash:sha256 \
     -- "$LOCAL_AI_JUDGE" \
       "$LOCAL_AI_PROMPT" \
       "$enriched/evidence.md" \
       "$enriched/context.md" \
       "$enriched/hf-metadata.md" \
       "$provider" "$model_id" "$endpoint" "$commentary" "$pi_state"
   ```
   Mechanics:
   - **Requires a parent Tally job**: the script hard-fails if
     `$TALLY_SOCKET`/`$TALLY_JOB_ID` are unset — it must itself be running as
     a Tally-admitted job to be allowed to enqueue a child.
   - **`--pool worker-gpu`**: the single scarce-resource pool this child
     leases (defined in `home/tally.nix` as `resource = "vram"; capacity = 1;
     enforce = "cooperative"; hardPreempt = false;`) — the *parent* holds no
     GPU pool at all, only the separate `local-ai-review` mutex pool (also
     `home/tally.nix`, `resource = "mutex"; capacity = 1`).
   - **`--priority low`**.
   - **`--dedup-key "local-ai-judge-$period-${evidence_digest:0:20}"`**: dedup
     key is a composite of the review period and the first 20 hex chars of a
     digest computed by hashing the sha256 sums of the three evidence files
     together (`evidence_digest="$(sha256sum "$enriched/evidence.md"
     "$enriched/context.md" "$enriched/hf-metadata.md" | sha256sum | cut
     -d' ' -f1)"`, line 257–258) — i.e. dedup is keyed on *content identity of
     the evidence bundle*, not just on period, so a rerun against unchanged
     evidence collapses to the same job.
   - **`--runtime-max-sec "$model_timeout"`**, sourced from the registry's
     `.limits.model_timeout_seconds`.
   - **`--no-enqueue`**: the child itself is forbidden from enqueuing further
     work (the inner Pi process never gets Tally access — matches the
     pattern doc's "an inner Pi agent never receives Tally access or
     enqueues surprise work").
   - **`--wait`**: the parent blocks synchronously until the child completes
     — this is the "blocking-runner-in-a-cheap-pool" architecture: the parent
     process itself just sits in a cheap wait, not holding the GPU pool,
     while a separately admitted child holds `worker-gpu` for exactly the
     duration of the Pi call.
   - **`--evidence exit:0`, `--evidence "artifact:$commentary"`, `--evidence
     hash:sha256`**: three declared evidence terms attached to the child job
     — exit code must be 0, the commentary file is captured as an artifact,
     and its hash is recorded (sha256) — this is Tally's proof mechanism, not
     ad hoc logging.
   - **Adapter**: the enqueued argv is `$LOCAL_AI_JUDGE` (a
     `writeShellApplication` wrapping `lib/judge.sh`), invoked with 9
     positional args: prompt path, three evidence file paths, provider,
     model_id, endpoint, output commentary path, and a per-run Pi state
     directory. `judge.sh` (`lib/judge.sh`) itself invokes Pi directly:
     ```bash
     PI_CODING_AGENT_DIR="$state_dir" \
     PI_TELEMETRY=0 \
     LLAMA_SWAP_URL="$llama_swap_url" \
     "$LOCAL_AI_PI" \
       --extension "$LOCAL_AI_PI_PROVIDER_EXTENSION" \
       --no-extensions --no-skills --no-prompt-templates --no-context-files \
       --no-session --no-approve --no-tools \
       --print --mode text \
       --provider "$provider" --model "$model" \
       "@$prompt" "@$evidence" "@$context" "@$hf_metadata" \
       'Write only the proposed pull-request commentary now.' \
       > "$temporary"
     ```
     then validates output size (40–50,000 bytes) and rejects any output
     containing the literal string `<!-- local-ai-monthly-state` (Pi is
     forbidden from writing workflow state into its own output — a
     prompt-injection/state-confusion guard).
10. **`finalize` Nix build** (lines 282–291): third `stages.nix` build,
    folding `commentary` into the final candidate (`next-sources.json`,
    `pr-body.md`).
11. **Preview early-exit** (lines 293–298): if not `--publish`, stop here.
12. **Publication** (lines 300–339): creates a disposable git worktree on a
    new branch `automation/local-ai-review-$period`, copies in exactly
    `pkgs/local-ai-monthly/sources.json` from the finalized output, asserts
    the staged diff touches *only* that one path (a hard "publication scope
    violation" check, line 306–309), runs `git diff --cached --check`,
    revalidates `schema_version == 1`, runs `nix build
    ...#local-ai-monthly` against the candidate tree as a build-level sanity
    check, commits, force-with-lease pushes (or sets upstream on first push),
    and creates/updates a GitHub PR via `gh pr create`/`gh pr edit` keyed on
    an existing open PR for that branch.
13. Final `status` values across the run: `running` (initial) → one of
    `no-delta`, `prepared`, `preview`, `published`, or `failed` (set by the
    exit trap on nonzero exit).

### `stages.nix` mechanics (the deterministic Nix "phase" derivations)

`pkgs/local-ai-monthly/stages.nix` is a single parameterized
`builtins.derivation` factory taking `phase` (`"prepare"|"enrich"|"finalize"`)
plus phase-specific args (`registry`, `capture`, optional `hfCapture`,
optional `commentary`). It reattaches the closure of pre-built store paths
(`bashPath`, `pureStagePath`) passed in as strings rather than actual
derivation attrsets — the comment explains why (line 12–13): "Reattach the
closure context of exact outputs embedded by the packaged supervisor.
Treating these paths as source trees drops runtime references." Each phase
just execs the shared `local-ai-monthly-pure-stage` binary
(`lib/pure-stage.sh`, built by `default.nix`) with the phase name and args;
`preferLocalBuild = true; allowSubstitutes = false;` — these are always
locally built, non-cacheable/non-substitutable derivations since they process
run-specific captured data, not fetched from a substituter.

### `default.nix` packaging shape

`pkgs/local-ai-monthly/default.nix` builds five separate
`writeShellApplication`s (`pureStage`, `capture`, `hfCapture`, `judge`,
`supervisor`) plus a `tallyEntry` wrapper (`local-ai-monthly-tally`, just
`exec local-ai-monthly --publish "$@"` — the exact argv the Tally producer
enqueues) and a `tests` derivation, then `symlinkJoin`s `supervisor` +
`tallyEntry` into one package whose `postBuild` runs the test suite
(`local-ai-monthly-tests`) as a build-time gate. All environment wiring
(`LOCAL_AI_CAPTURE`, `LOCAL_AI_JUDGE`, `LOCAL_AI_PROMPT`, `LOCAL_AI_STAGES`,
etc.) is injected as `export` lines into the wrapper script rather than
passed as CLI flags — this is how Nix store paths reach the bash supervisor
without the supervisor.sh script itself needing to know about the Nix build
graph.

### The Tally producer wiring in `home/tally.nix`

Lines 133–159:

```nix
monthly-local-ai-review = {
  kind = "calendar";
  onCalendar = "*-*-01 00:30:00";
  enqueue = {
    argv = [
      "${pkgs.local-ai-monthly}/bin/local-ai-monthly-tally"
      "--tally"
      "${tallyPackage}/bin/tally"
    ];
    pool = "local-ai-review";
    priority = "low";
    dedupKey = "monthly-local-ai-review-%Y-%m";
    evidence = [
      "exit:0"
      "artifact:/home/tom/.local/state/local-ai-monthly/last-run.json"
      "hash:sha256"
    ];
    runtimeMaxSec = 43200;
    noEnqueue = false;
  };
};
```

The parent producer takes only the `local-ai-review` mutex pool (`resource =
"mutex"; capacity = 1`) — never `worker-gpu`. `noEnqueue = false` on the
*parent* is the deliberate complement of `--no-enqueue` on the *child* it
spawns: the parent is explicitly granted child-enqueue capability, and Tally
injects `TALLY_SOCKET`/`TALLY_JOB_ID` for that one nested call
(`home/tally.nix` comment, lines 136–138: "noEnqueue=false is the deliberate
child capability; Tally injects the parent identity and socket for that
call."). The code comment directly above the pools block (lines 87–88) is
the general framing: "These are real contention lanes, not synthetic
maintenance pools. All are centrally owned even when their physical resource
is on worker."

The doc-level summary of this exact mechanism, `docs/local-ai/monthly-workflow.md`
line 35–40:

> The calendar parent holds only the `local-ai-review` mutex. Deterministic
> Git, Nix, HTTP, and publication work therefore does not reserve VRAM. The
> parent is allowed one child enqueue; that low-priority child alone holds
> `worker-gpu` for the Pi process and releases it immediately afterward. This
> uses the same Tally calendar-to-opaque-argv shape as the nightly fleet
> updates, with a nested lease because only one stage consumes the scarce
> resource.

And the pattern doc's general statement of the architecture,
`pi-appliance-pattern.md` lines 120–132 ("Tally and compute ownership"):

> Tally still schedules one executable. A workflow that consumes one fixed
> resource set declares it before admission. A linear workflow with
> substantial non-GPU work may instead hold its own workflow mutex and
> enqueue a declared, bounded child for the GPU-only stage, as the monthly
> source review does. The parent identity, dedup key, depth/fanout caps, and
> `noEnqueue` capability make that child explicit; an inner Pi agent never
> receives Tally access or enqueues surprise work.
>
> Git, deterministic transforms, Pi processes, validation, and publication
> run on the coordinator unless a workflow explicitly declares another
> execution host. Model calls cross only the llama-swap boundary to the
> selected compute host.

### How the PR is produced (recap, precise)

Only `pkgs/local-ai-monthly/sources.json` is ever staged/committed by the
automation (`supervisor.sh` lines 300–309, hard scope-violation assertion).
The commit message is fixed: `"chore(local-ai): advance $period review
pins"`. Branch: `automation/local-ai-review-$period`. On rerun, push uses
`--force-with-lease=refs/heads/$branch:$remote_sha` if the branch already
exists remotely, else a plain `--set-upstream` push. PR title is fixed:
`"chore(local-ai): $period source review"`; if an open PR already exists for
that branch, its title/body are edited in place (`gh pr edit`) rather than a
new PR opened; otherwise `gh pr create`. Body content always comes from
`$finalized/pr-body.md`, a deterministic Nix-built artifact — Pi's commentary
is folded into this body but Pi itself never touches Git/GitHub (per
`monthly-workflow.md` line 92: "Pi writes one bounded Markdown commentary
file. It cannot see the publication worktree and cannot call Git or
GitHub.").

---

## 5. The "Swarm" section, quoted in full

Source: `docs/local-ai/pi-appliance-pattern.md`, lines 88–106.

> ## Swarm: typed stages, not free-form delegation
>
> A swarm composes the same primitive as a small declared DAG. Each stage has
> its own skill, input schema, output schema, tool list, selector, quota, and
> reducer. For example, academic OCR might use:
>
> ```text
> document partition
>   → pooled-fast visual extractors
>   → deterministic layout/table normalization
>   → pooled-fast discrepancy critics
>   → strongest reconciliation aggregator
>   → deterministic page/result validation
> ```
>
> Stages communicate only through validated artifacts. A parent does not
> expose a child's specialist tools to itself, and a child does not inherit
> general shell or network access. This is procedural fan-out/fan-in, not an
> unconstrained chat between agents.

Notes for the flow dialect:

- A swarm is explicitly framed as "the same primitive" as a pool, just
  composed into "a small declared DAG" — i.e. tally-flow does not need a
  separate primitive for swarms; a swarm is a sequence/DAG of stages where
  each stage is itself a (possibly pooled) map/validate/reduce unit.
- Each stage independently declares: skill, input schema, output schema, tool
  list, selector (which may itself be a pool selector, as in the example
  where two of five stages are `pooled-fast`), quota, and reducer. This is
  the full per-stage declaration surface the flow DSL needs to support.
  Non-model stages (`deterministic layout/table normalization`,
  `deterministic page/result validation`) sit in the same DAG as model
  stages — the DAG is not model-calls-only.
  The example DAG's final aggregation step uses `strongest` (single member,
  `identity` reducer implied) rather than a pool, for the reconciliation
  stage — pooling is not mandatory at every stage.
- Isolation is stage-to-stage, not just member-to-member within one stage:
  "A parent does not expose a child's specialist tools to itself, and a
  child does not inherit general shell or network access." This is a
  stronger claim than plain data-flow isolation — it is also a *tool/capability*
  isolation rule across the DAG edges.

---

## 6. Other constraints on pooled execution (thermal/cooldown, VRAM, llama-swap catalog)

### VRAM/pool capacity model (`home/tally.nix`, lines 89–114)

All GPU-shaped pools in the shipped Tally config are `capacity = 1` (single
concurrent holder), `enforce = "cooperative"`, `hardPreempt = false`:

```nix
build = { resource = "build-slot"; capacity = 1; enforce = "cooperative"; hardPreempt = false; };
coordinator-gpu = { resource = "vram"; capacity = 1; enforce = "cooperative"; hardPreempt = false; };
worker-gpu = { resource = "vram"; capacity = 1; enforce = "cooperative"; hardPreempt = false; };
local-ai-review = { resource = "mutex"; capacity = 1; enforce = "cooperative"; hardPreempt = false; };
```

There is exactly one VRAM lane per host (`coordinator-gpu`, `worker-gpu`),
each capacity 1 — meaning a pool of N model members sharing a single
llama-swap-backed host cannot run N members truly concurrently on GPU under
this pool model as configured; they would have to be serialized through the
single `*-gpu` pool slot (or the flow would need per-member sub-scheduling
inside one held lease, which is out of scope of what's shown here). This is
an important constraint for tally-flow's pool executor: with today's Tally
pool config, "pooled" fan-out over local GGUF models is concurrency-1 at the
resource-admission layer, however many `count` the selector asks for — pool
parallelism, if wanted, would have to be layered as sequential leases within
one held pool slot, not simultaneous slot holders.

### Thermal/cooldown rule (`home/tally.nix`, lines 42–73)

The `cooldownReceiver` (`tally-gpu-cooldown`) is a hardware-tripwire-driven
Tally enqueue, not part of the monthly workflow, but it establishes the
existing thermal-guard pattern any pooled/GPU-heavy flow must coexist with:

> Fixed receiver used by worker's hardware tripwire. The sleep runs locally
> on coordinator: it needs only to hold the logical worker-gpu gate for 30
> minutes. Interrupt priority makes it next, while hardPreempt=false means it
> never kills the current GPU holder.

Mechanically it enqueues (on trigger from worker's sensor) a `sleep
$seconds` job into `worker-gpu` at `--priority interrupt` with dedup key
`gpu-cooldown-worker-${sensor_kind}-${temp_c}C-${stamp}` and `--no-enqueue`,
`--evidence exit:0`. Because `hardPreempt=false`, it does not kill an
in-flight pool member — it only claims the *next* slot, so a running pooled
inference is not interrupted mid-flight by a thermal event, but the next
member/stage will queue behind the cooldown sleep. Any flow-level pool
executor holding `worker-gpu` across a multi-member fan-out should account
for this: cooldown is "next in line," not "preemptive," under current pool
semantics.

### VRAM budget figures (`docs/local-ai/model-roster.md`, lines 1–12)

> The Nix catalog contains 12 deployment rows and 15 pinned files totaling
> 365,900,189,792 bytes (340.77 GiB). If the global gate were lifted without
> a placement change, the current projection would root 273,678,128,448 bytes
> on the coordinator and 365,900,189,792 bytes on the exhaustive worker.
> Those figures exclude FastFlowLM-owned NPU weights and the speech
> appliances. This is one reason the gate must remain closed until the
> deployment pass.

This is a static roster-size figure, not a live VRAM accounting mechanism —
there is no dynamic VRAM-budget enforcement shown anywhere in these files
beyond the `capacity = 1` mutex-style pool gating above. `ramTierGb` on each
deployment row (`lib/local-models.nix`) is informational sizing metadata, not
wired into any admission-control check in the reviewed files.

### llama-swap catalog mechanics

- All llama.cpp-backed rows pin one exact upstream runtime commit:
  `ggml-org/llama.cpp@571d0d5` (`571d0d540df04f25298d0e159e520d9fc62ed121`),
  set once (`llamaCppCommit`, `lib/local-models.nix` line 47) and reused via
  the `llamaCppRuntime` helper across every Vulkan/ROCm deployment row.
- Non-llama.cpp peers (currently only FastFlowLM's NPU-hosted
  `gemma4-it:e4b`) are fronted through llama-swap via a `peer` block (`name`,
  `proxy` base URL, optional `systemdUnit`) rather than a `runtime` block —
  callers still "enter through llama-swap" uniformly regardless of backend
  (`model-roster.md` line 23, deployment notes: "FastFlowLM owns these
  weights via runtime flm pull; callers still enter through llama-swap").
- The monthly workflow's live-catalog join queries llama-swap's
  OpenAI-compatible `/v1/models` endpoint directly (`supervisor.sh` lines
  204–212), constructed from the registry's `.inference.url` with a `/v1`
  suffix normalized on if missing — this is the exact shape a flow-host
  "what's actually being served right now" check should reuse.
- Deployment `status` distinguishes `canonical` from `candidate` /
  `experimental` / `negative` / `retired`, and `evidence` separately
  distinguishes `matched-local` / `upstream-measured` / `api-only` /
  `unverified` — pool/selector resolution presumably should restrict to
  `canonical` rows at minimum (not stated as an explicit filter rule in a
  single place, but implied throughout — e.g. `qwen3-vl-8b-ocr` and other
  currently-served rows are all `status = "canonical"`).
- **Global fetch gate**: `services.local-models.downloadAllModels` stays
  `false` fleet-wide; the entire catalog above is metadata-only until Tom
  manually lifts that gate (`docs/local-ai/README.md` lines 7–13,
  `lib/local-models.nix` lines 330–332 code comment). No workflow — monthly
  review included — is permitted to flip this gate
  (`monthly-workflow.md` line 122: "The workflow never merges its own PR,
  edits `lib/local-models.nix`, downloads weights, changes
  `downloadAllModels`, or deploys a service.").

---

## Summary for the spec author

- The pool selector grammar (`pooled-fast(N, diversity=...)` /
  `pooled-strongest(N, diversity=...)`) is small, closed-vocabulary at the
  class level (`fast`, `strongest` today), open at the diversity-key level,
  and its resolved member list is a **witness-write-before-inference**
  requirement — treat that ordering as a correctness invariant, not a nice-to-have.
- Map/validate/reduce is: fresh process per member, shared immutable
  evidence, zero cross-member visibility, schema+provenance gate before
  reduction, three reducer classes (`identity`, `deterministic`,
  `pi-aggregate(<class>)`) with `pi-aggregate` requiring
  support/conflict/excluded dissent attribution in its output — never plain
  majority-collapse.
- Quorum is three explicit fields (`required_members`, `minimum_valid`,
  partial-reduction-allowed), one repair attempt for members and reducers
  alike, and hard fail-closed (no durable-state advance) below quorum, with
  a permitted degraded-but-honest human briefing.
- The roster (`lib/local-models.nix` + `model-roster.md`) cleanly separates
  `artifacts` (weights/files) from `deployments` (servable rows); a flow
  catalog schema needs at minimum: model ID, role, status, evidence tier,
  backend, hosts, and lineage/maker/base-checkpoint fields to satisfy the
  documented diversity keys — and ensemble/pool policy is explicitly a
  caller-side concern, never hidden in the roster.
- `supervisor.sh` is the one shipped instance of the
  blocking-runner-in-a-cheap-pool shape tally-flow must generalize: parent
  holds a cheap mutex pool only, does all deterministic prep via sandboxed
  Nix derivations, then does one `tally enqueue --pool worker-gpu --wait
  --no-enqueue --dedup-key <content-hash-derived>` for the sole GPU-bound
  step, with three declared evidence terms (`exit:0`, `artifact:<file>`,
  `hash:sha256`), and finally produces a PR touching exactly one file, gated
  by an explicit staged-path allowlist check.
- Today's Tally pool config gives every GPU-shaped pool `capacity = 1`, so
  "pooled" execution across members is currently concurrency-1 at the
  resource layer regardless of selector `count` — worth flagging explicitly
  in the flow spec as a scheduling gap pooled selectors will surface, not
  something already solved by existing pool primitives.
