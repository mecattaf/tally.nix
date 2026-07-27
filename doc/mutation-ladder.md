# Mutation-ladder cookbook

The [academic OCR flow](../examples/flows/academic-ocr.js) demonstrates a general repair pattern:
when independent methods disagree, change the input in a bounded, deterministic way before asking
the methods again. Do not call the same method with the same input and label the duplicate work a
repair.

The flow's atomic work item is a `(paper, page, protocol)` triple. The page list and protocol
roster are ordinary `args`, so enumeration is replay-stable data rather than a filesystem scan or
clock read inside the JavaScript host. If another process discovers pages, it should first produce
that exact list as a witnessed result or immutable manifest; the OCR run consumes the resulting
data.

## The pattern

A mutation ladder has five parts:

1. Run independent methods against one input, starting with the cheapest tier.
2. Measure disagreement in deterministic code over compact witnessed summaries.
3. If the summaries disagree, derive a different input from the verdicts and run the methods
   again under new keys.
4. Stop at the first accepted agreement or at an explicit iteration limit.
5. Send only the bounded residue to an arbiter.

In the OCR example, tiers are `cheap`, `standard`, and `specialist`. Each tier is a
`parallel()` fan-out. The flow compares bounded signature vectors itself; an LLM does not decide
whether to escalate. The three input mutations are ordered:

- re-rasterize the page at the configured DPI;
- crop the sorted, de-duplicated hot zones reported by the preceding witnessed attempts; and
- deskew by the deterministic median of the preceding skew estimates.

Every recognition node receives the original page path plus one explicit input variant. Its
flow-local key contains the paper, page, protocol, and variant. A re-rasterized attempt can
therefore never be mistaken for a retry of the original input, and replay checks the derived
payload at the same ordinal.

Plain retry is appropriate for a transient transport or process failure when the intended work is
unchanged. It is not a disagreement strategy. A deterministic OCR protocol given the same bytes
and configuration is expected to produce the same answer; repeating it creates another cost
without adding a new hypothesis. A mutation ladder instead asks whether the method becomes stable
under a declared transformation. The witness chain then records which protocol/input combinations
were tried, how long each held its lease, which artifact each proved, and which combination was
selected.

## Driver contract and frame discipline

The JavaScript controls routing but does not move page content. One driver program implements two
direct actions:

| Action | Input in `TALLY_BRIEF` | Structured result |
|---|---|---|
| `recognize` | Page identity and source path, protocol ID/tier, mutation descriptor, and the exact output path. | Echoed identities, artifact path, text digest, a signature of at most 32 integers, confidence, at most eight hot zones, and a skew estimate. |
| `arbitrate` | Page identity, the original source path, compact summaries of the exhausted attempts, and the exact output path. | Final artifact path and digest plus the artifact paths used as its basis. |

The adapter must scrape the compact JSON line as `finalMessage`. The OCR text, page image,
token-level confidence data, and mutated rasters stay below `outputDir`. Each child declares
`artifact:<path>` plus `hash:sha256`, so the witness proves the referenced file. The flow's final
value contains one compact page summary: chosen path/digest, resolution route, disagreement score,
attempt count, and the selected node's task UUID and witness sequence.

This is the intended response to the symmetric 16 MiB NDJSON frame limit. The limit applies to
requests as well as responses; it is not a target size for carrying OCR blobs. Arbiter briefs also
contain paths and digests rather than concatenated OCR text. Cross-host deployments must put
artifacts in the workspace or artifact store because tally has no hidden shared-filesystem data
plane.

A suitable adapter shape is:

```nix
services.tally.adapters.ocr-driver = inputs.tally.lib.adapters.mkAdapter {
  argv = [ ];
  scrape.finalMessage = inputs.tally.lib.adapters.mkScrapeCapture {
    pattern = "^TALLY_FINAL_MESSAGE=(.*)$";
  };
};
```

The driver is invoked directly as `<program> recognize` or `<program> arbitrate`; it reads the
structured brief and prints one final line:

```text
TALLY_FINAL_MESSAGE={"paperId":"paper-a","pageNumber":1,...}
```

## Registration and node ceilings

The shipped schema permits at most 100 pages, four protocols, and three mutation iterations. Its
worst-case node count is:

```text
pages × protocols × (original input + mutation iterations) + pages × arbiter
100   × 4         × (1 + 3)                              + 100 = 1700
```

Accordingly, both `meta.maxNodes` and the registration's `maxNodes` are 1700. The module checks
that the registration covers the script declaration before activation. If a copy of the pattern
changes any of the three bounds, recalculate both values from the same formula. `maxNodes` is the
whole-run fuse: it stops a bad branch from materializing an unbounded number of separately leased
jobs, regardless of which call site produced them.

The recognition `job()` call site can execute at most `100 × 4 × (1 + 3) = 1600` times, so the
example declares `iterationCap: 1600`. That cap is per host call site. It catches an accidental
control-loop expansion at the recognition site; it does not replace `maxNodes`, because arbiters
come from another call site. The algorithm also has the tighter domain cap
`maxMutationIterations <= 3`, which is what makes the repair ladder terminate deliberately.

For example:

```nix
{
  services.tally = {
    enable = true;

    pools.ocr-gpu = {
      resource = "vram";
      capacity = 1;
    };

    flows.academic-ocr = {
      script = "${inputs.tally}/examples/flows/academic-ocr.js";
      maxNodes = 1700;
      args = {
        pages = academicPages;
        protocols = [
          { id = "fast-layout"; tier = "cheap"; }
          { id = "fast-text"; tier = "cheap"; }
          { id = "vision-ocr"; tier = "standard"; }
          { id = "formula-ocr"; tier = "specialist"; }
        ];
        driver = {
          adapter = "ocr-driver";
          program = "${pkgs.academic-ocr-driver}/bin/academic-ocr-driver";
          runtimeMaxSec = 1800;
        };
        outputDir = "/var/lib/academic-ocr/runs/2026-07";
        rasterDpi = 400;
        maxMutationIterations = 3;
        maxDisagreementPermille = 100;
      };
    };
  };
}
```

The example needs only the ordinary co-residency `ocr-gpu` pool requested by each child. Each
triple releases that lease when it finishes. An interrupt-priority thermal cooldown can therefore
take the GPU between triples; the flow experiences the interruption as a longer await and resumes
without special control logic.

## What 400 nodes actually means

With 100 pages and four protocols, a path that reaches every tier materializes 400 recognition
nodes for the original input. All are low priority. The lease scheduler groups child requests by
`flowRunId` and braids groups at the same effective priority: it takes the first request from each
flow or standalone group before the second request from any group, then the third, and so on. A
400-deep OCR group therefore cannot starve a smaller sibling flow. After the configured aging
threshold, a waiting request advances exactly one priority class; aging and the per-run braid are
separate fairness mechanisms.

The JavaScript can hold 400 branch promises, but the runner uses one multiplexed daemon
connection. The server services at most 64 requests on that connection at once. Long-lived
`queue.await_job` calls fill that window; later admissions remain behind the connection's
arrival-ordered backpressure until a request completes. The number 64 is a transport window, not a
second node ceiling and not GPU concurrency. Pool capacity still decides how many admitted OCR
jobs run, while `maxNodes` counts every materialized node over the complete run.

The upper bound remains 1700 because any disagreeing page may run the same protocol roster over
three new inputs and then use one arbiter. In a normal run, early agreement skips later tiers,
mutations, and arbitration, so the observed count is lower. The final
`configuredNodeUpperBound` field reports the bound for the supplied page/protocol counts and
mutation limit.

## Applying the pattern outside OCR

Suppose three importers disagree about a legacy tabular file. Retrying each importer against the
same bytes is not useful. A mutation ladder can instead:

1. normalize line endings;
2. transcode from the witnessed candidate encoding; and
3. apply an explicit delimiter/quote dialect derived from the prior parse summaries.

Each importer/input pair writes its full parsed table to an artifact and returns only a schema
digest, row count, rejected-row count, and a bounded disagreement signature. Plain code compares
those summaries. Agreement selects an artifact; exhausted disagreement invokes a bounded human or
specialist parser node. The same construction applies to compiler differential testing, document
conversion, media decoding, and any problem where a declared input transformation explores a new
hypothesis.

Use the pattern when mutations are deterministic, independently meaningful, and cheap enough to
bound in advance. If the next action requires an open-ended policy choice, put that choice in a
driver or stop for an operator; do not disguise it as another ladder rung.

## Executable reduced run

`crates/tally-flow/tests/academic_ocr.rs` runs the example through the real deterministic engine
with a stub protocol client. Two pages start with disagreeing cheap protocols and escalate to a
specialist. Both take exactly one re-rasterization iteration: one page then converges, while the
other remains disputed and creates exactly one arbiter node. The test also checks per-triple key
uniqueness, low-priority GPU admission, hashed artifact evidence, deterministic page order, and
compact arbiter inputs.
