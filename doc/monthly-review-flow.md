# Monthly review flow

`examples/flows/monthly-review.js` is the flow-era migration of the
`local-ai-monthly` supervisor. The supervisor remains the reference implementation until
one complete calendar period has run green under the flow. Do not remove it as part of
registration or preview testing.

## Behavioral contract

The flow preserves the existing publication boundary:

- period branch: `automation/local-ai-review-YYYY-MM`;
- title: `chore(local-ai): YYYY-MM source review`;
- the only committed path: `pkgs/local-ai-monthly/sources.json`;
- no source-pin delta: finish as a census without a pull request;
- preview mode: produce the same finalized candidate without pushing;
- publication mode: create or update the one period pull request; and
- fixed receipt: schema 2 `last-run.json`, with no model blobs.

The deterministic `prepare`, `enrich`, and `finalize` stages remain the existing
`builtins.derivation` stages. A workload driver evaluates each runtime-dependent
derivation and returns its `{ drvPath, outputs }` description; the flow then executes it
with `drv()`. The driver must not build those derivations itself.

The model phase resolves:

```js
members("pooled-strongest", { count: 3, diversity: "maker" })
```

and fails before inference unless exactly three members resolve. All three initial calls
are submitted through `parallel()`. Invalid, failed, or missing members get one ordinary
repair node and no second repair. The final quorum uses the configured `minimumValid`.
The reducer is schema-validated, gets at most one repair, and its conclusions must
attribute both support and conflict to valid member IDs. The pull-request commentary
contains the reducer text, every accepted model's full commentary under its member ID,
the dissent ledger, and any excluded members.

## Workload-driver boundary

The flow keeps impure Git, HTTP, filesystem, and GitHub work outside the deterministic
JavaScript host. `args.driver.adapter` must scrape the compact JSON result emitted by
`args.driver.program` as `finalMessage`. A suitable adapter shape is:

```nix
services.tally.adapters.monthly-review-driver = inputs.tally.lib.adapters.mkAdapter {
  argv = [ ];
  scrape.finalMessage = inputs.tally.lib.adapters.mkScrapeCapture {
    pattern = "^TALLY_FINAL_MESSAGE=(.*)$";
  };
};
```

The program is invoked directly as `<program> <action>`, reads the structured
`TALLY_BRIEF`, and prints one final line:

```text
TALLY_FINAL_MESSAGE={"period":"2026-07",...}
```

It has five actions:

| Action | Required behavior | Structured result |
|---|---|---|
| `capture` | Reuse the supervisor's clone, source capture, catalog/model census, accepted-tally capture, receipt initialization, and `prepare` evaluation. The optional period is resolved here in `Europe/Paris`; that witnessed result pins replay. | Period, change count, commit, run/receipt/commentary paths, and the `prepare` derivation. |
| `enrich` | Run the same bounded Hugging Face capture, evaluate `enrich`, and read the model endpoint/timeout plus exact evidence paths from its realized output. | Evidence digest and paths, provider/model/endpoint/timeout, and the `enrich` derivation. |
| `finalize` | Atomically write the attributed commentary, enforce the existing 40–50000 byte and state-marker checks, and evaluate `finalize`. | Commentary path and the `finalize` derivation. |
| `publish` | Reuse the supervisor's one-file staging check, flake build, commit, force-with-lease update, and create-or-edit pull-request logic. Update the schema 2 receipt atomically. | Status, period branch/title, exact changed path, and nullable pull-request URL. |
| `failure` | Atomically record the original flow error in the fixed receipt without masking that error. | `failed` and the receipt path. |

The driver should be a refactor of the shipped supervisor functions, not a second
implementation of their policy. Its `capture` and `enrich` results are compact metadata;
the evidence files stay at their absolute paths and model blobs never enter the flow
result.

Catalog members must use a noninteractive local adapter with a `finalMessage` scrape and
the same restrictions as the current judge: llama-swap only, no tools, extensions,
skills, prompt templates, context files, approval, or session. The member's static
`launch.model` selects the model. The flow supplies the witnessed endpoint through
`LLAMA_SWAP_URL`, a per-member state directory, the original review/evidence/context/HF
paths in the mission, and the existing timeout.

## Registration

Render the catalog from the roster with `lib.tally.mkCatalog`; do not hand-write catalog
JSON. The scheduled registration retains the old period identity and parent evidence:

```nix
services.tally.flows.monthly-review = {
  script = "${inputs.tally}/examples/flows/monthly-review.js";
  onCalendar = "*-*-01 00:30:00";
  dedupKey = "monthly-local-ai-review-%Y-%m";
  runtimeMaxSec = 43200;
  maxNodes = 16;
  catalog = monthlyReviewCatalog;
  evidence = [
    "exit:0"
    "artifact:/home/tom/.local/state/local-ai-monthly/last-run.json"
    "hash:sha256"
  ];
  args = {
    minimumValid = 2;
    publish = true;
    dotfilesUrl = "https://github.com/mecattaf/dotfiles.git";
    baseBranch = "main";
    driver = {
      adapter = "monthly-review-driver";
      program = "${pkgs.local-ai-monthly-flow-driver}/bin/local-ai-monthly-flow-driver";
      stateDir = "/home/tom/.local/state/local-ai-monthly";
      receiptPath = "/home/tom/.local/state/local-ai-monthly/last-run.json";
      runtimeMaxSec = 43200;
    };
  };
};
```

For same-period comparison, set `args.period` explicitly and run with
`publish = false`. Compare the finalized `next-sources.json` and `pr-body.md` with the
supervisor outputs before enabling the timer. Switch calendar ownership only after that
preview is equal. Keep the supervisor packaged and in version control until the first
full period's branch, pull request, receipt, and witnesses are green.

## Deduplication and replay

The calendar parent keeps `monthly-local-ai-review-%Y-%m`. Model keys are:

| Node | Global dedup key |
|---|---|
| First selected member | `local-ai-judge-<period>-<evidence-digest-20>` |
| Other initial members | the same base plus `-<member-id>` |
| Member repair | the same base plus `-<member-id>@1` |
| Reducer | `local-ai-reduce-<period>-<evidence-digest-20>` |
| Reducer repair | the reducer key plus `@1` |

The first key retains the single-model supervisor identity; new pooled work is
unambiguous. A restarted runner replays capture, `drv()` stages, and completed members
as terminal/substituted history. It reaches the first unfinished ordinal without
re-running those model processes.

## Primitive audit

The migration wanted these primitives and found them:

- custom adapters plus structured briefs/results for the workload driver;
- `drv()` for all three store-native stages;
- catalog rendering from the roster and deterministic maker-diverse selection;
- `parallel()`, `attributed()`, `quorum()`, `dissent()`, and explicit repair nodes;
- global dedup keys for cross-run period/evidence reuse; and
- stateless runner replay from terminal witnesses.

It exposed these platform gaps:

- [#104](https://github.com/mecattaf/tally.nix/issues/104): the daemon acknowledged a
  live terminal result before `finalMessage` reached `NodeResult`, and replay omitted it.
  The minimum result-join path required by this migration is included here; the broader
  timeout/query audit remains open.
- [#105](https://github.com/mecattaf/tally.nix/issues/105): scheduled flows could not
  override their daily dedup template. This migration adds the typed `dedupKey` option
  while retaining the daily default.
- [#106](https://github.com/mecattaf/tally.nix/issues/106): scheduled flows discarded
  parent artifact evidence. This migration adds the typed `evidence` option while
  retaining `["exit:0"]` by default.
- [#107](https://github.com/mecattaf/tally.nix/issues/107): a flow runner cannot hold the
  workload's capacity-1 mutex for its complete lifetime. Same-period duplication remains
  excluded by the parent key, but stronger cross-period/manual mutual exclusion is not
  equivalent and needs a ruling before the runner lease surface changes.
- [#108](https://github.com/mecattaf/tally.nix/issues/108): catalog checking proves that
  a selector class is nonempty, not that a literal `count: 3` request has three members.
  The workload fails closed before inference; activation-time cardinality proof remains
  follow-up work.

The period clock is not a missing host primitive. `Date` remains forbidden in the
dialect; the impure capture node resolves the period once, witnesses it, and replay
consumes that recorded result.
