I have what I need. The repo itself turns out to be the strongest evidence source — the flake already practices several of these idioms unnamed. Writing the report now.

---

# SDD native to Nix: what the mechanism is, for tally's house format

## 0. The ground truth this report stands on

Before mining external analogies, an observation from reading `flake.nix` (4,522 lines): **tally already practices proto-Nix-native SDD in at least six places, without naming it**. These are not proposals; they are committed, consumed bytes today:

- `hardening-doc-drift` — a derivation that extracts every backtick-quoted property from a doc page and greps it against executor source. Prose pinned to code by a check.
- The `documentation` derivation — option reference pages **rendered** from `nixosOptionsDoc`, with a guard that *fails the build if a generated page is checked in*, plus `jq` assertions over the options JSON (key parity between core/HM/NixOS surfaces, a forbidden-argv scan of the rendered examples).
- `mkCatalogRejectionCheck` — eval-time assertions with the **exact expected failure message** asserted, i.e. negative fixtures for a Nix-level schema.
- `pkgs.testers.testBuildFailure'` used ~17 times — perturbation probes proving checks can fail, as a standing pattern.
- `poolResourceDeclarationFixture` — one checked-in golden JSON that Nix rendering must produce and a Rust `Deserialize` test must read. Cross-language contract as a shared byte fixture, double-pinned. The comment says it precisely: "Nix's rendering and Rust's reading of it cannot drift apart silently without both pins failing."
- The epsilon-extension worklist's `flake-build-subset` gate: a witnessed argv whose body is `nix build .#checks.x86_64-linux.<attr>...` — the gate/check composition already exists.
- The `spec-build-driver` already emits `source.path`, `source.sha256`, `source.revision` in the worklist it derives — the pin chain exists in embryo.

The house format's job is not to invent a Nix-native mechanism from nothing. It is to **name, generalize, and mandate what the flake already does**, and to extend the pin chain one level up to the spec. That is the honest version of "native": spec-kit is bolted to its scripts; Kiro to its IDE; tally's spec layer should be bolted to the flake's check surface, because that surface is already the only thing in the repo that never lies.

---

## 1. The NixOS module system as spec-language precedent

**What an option declaration is.** `mkOption { type; description; default; }` plus module `assertions` is a complete specification *of a configuration space*: typed input surface, documented meaning, refusal of unknown keys, cross-cutting invariants with error messages, and a reference manual rendered from the declarations. Tally consumes this machinery heavily (the whole `tallyCoreOptions`/`producerOptionsDoc` apparatus).

**The option/requirement line.** An option is a *machine-set value*: it names a point in input space and its type closes that space (`producer.kind` must be one of two strings; unknown keys refused — the same law as the worklist's D77). A requirement is a *behavior claim over trajectories*: "WHEN a steer arrives THE SYSTEM SHALL refresh the epoch." The module system's deep lesson is that it never tries to express the second kind as the first. Types close input spaces; they cannot close behavior. So: **the spec layer should have a declared schema for its own structure** (section grammar, ID format, status block fields, traceability rows — all input-space facts, all refusable), and should *not* attempt a typed behavior language. EARS prose with stable `N.M` IDs plus an executable oracle per criterion is already the right division: the ID and the oracle binding are schema; the SHALL clause is law.

**Rendered-from-structured, or authored + linted?** The draft `specs/README.md` chooses authored-md + linter, and for the law sections that is right — humans ratify prose, and rendering law from JSON inverts authority the wrong way. But tally's own `documentation` derivation shows the correct refinement: **split the artifact by section**. Law sections (destination, rulings, requirements, unchanged behavior) are authored and linted. *Join* sections — the traceability table, coverage indexes, "which task discharges which criterion" — are **rendered from the parse, and forbidden from being checked in**, exactly as the flake already forbids checking in generated options pages ("generated options page must not be checked in"). A hand-maintained traceability table is a transcription act at spec altitude; ext0's whole destination is zero transcription acts. The table in `spec.md` §7 as drafted should be demoted from an authored section to a rendered artifact of the lint derivation.

**Assertions transfer directly.** Module assertions = the spec linter's cross-reference rules, with `mkCatalogRejectionCheck`'s discipline applied: every defect class the linter claims to catch gets a checked-in negative fixture that must fail *with the stated message*. Kiro's four-defect taxonomy (wrong level, ambiguity, inconsistency, incompleteness) splits here: the first is partially mechanical (implementation-language lexicon scan), the middle two are mostly semantic (grind territory), incompleteness of *structure* (a criterion with no oracle binding, a requirement with no criteria) is fully mechanical.

**What does not transfer.** Two things, firmly. The module system's merge semantics (`mkDefault`/`mkForce`, priority resolution) — spec law must not have override precedence; a ruling supersedes by freezing its predecessor with a pointer (the draft already says this). And the config fixpoint (config defined in terms of config) — a spec must resolve acyclically at commit time; anything else makes ratification unreviewable.

---

## 2. Flakes as the authority model

**`nix flake lock` is ratification, mechanized.** The lock file is a human act (running the command, committing the result) that produces machine-consumable pins (rev + narHash) which every subsequent evaluation trusts without re-asking. This maps one-to-one onto the spec lifecycle's missing piece. Today ratification is "an operator act, recorded in the status block" — prose. The Nix-native form: **ratification writes a pin** — the spec blob's sha256 at the admitted revision — into the machine-consumable chain. The chain already has its lower links: the derived worklist carries `source.sha256` (the driver emits it today), and ext0's `receipt-authority-stamp` puts `worklistSha256` on every receipt. Add one link — the worklist records the ratified spec sha it was derived from — and the full lineage *spec sha → worklist sha → receipt → release* is committed hashes end to end. "Which spec graded this code" stops being a claim and becomes a two-hop lookup. This is the direct application of the pin doctrine tally already holds ("rev+narHash is the contract, in place of version numbers") to its own authoring layer.

**`nix flake check` is the conformance surface.** The spec linter belongs at `checks.<system>.spec-lint` — not a script, not a skill step, a derivation. Then A15 (every verification artifact joins a standing gate or dies) is satisfied for the spec layer by construction, and the anti-rot rule's standing consumer for `spec.md` is nameable in one word: the check attribute. The grind's bar rotted for five days because its flake attribute executed nothing (`--list`-only, VD-5/F33); the countermeasure is the `testBuildFailure'` pattern: `spec-lint` ships with fixture specs that must fail, so the bar is shown to bite in the same eval that trusts it.

**Hermetic evaluation as the no-mutable-state rule.** A spec must resolve — every readFirst pointer, every evidence ID, every contract reference — from committed bytes at one revision, no network, no working tree. The sandbox enforces this for free: a `runCommand` over `self` physically cannot consult anything else. D68 (pointers name files existing at the authority revision; the 48 phantom pointers) becomes not a doctrine to remember but a property of where the check runs.

**The fileset is the consumer registry.** This is the subtlest and, I think, most genuinely Nix-native find. `tallySource` is a curated `lib.fileset` — files enter the build closure only when named, and the flake's comments already articulate why ("a doc page a Rust test reads is a fixture, and the packaged build cannot see one that is not named here"). Invert that into a law for the spec layer: **a spec artifact is consumed if and only if some check's fileset (or gate argv) names it, and the lint enumerates `specs/**` and fails on any file no consumer names.** The anti-rot rule — "an artifact whose consumer disappears is deleted, not kept" — becomes decidable instead of a review habit. Nix forces explicit enumeration of what builds can see; nothing else in the toolchain (not git, not a script harness, not an IDE) gives you unconsumed-artifact detection as a build failure.

**Frozen-flow when the spec joins the graded closure.** "The deployed store path grades, permanently" gets a sharp corollary: whatever spec-derived fixtures are inside the grading closure at deploy time are the ones that grade, regardless of later spec edits. This is correct and should be embraced, with one discipline: the *product* build's fileset should include spec bytes only where a test consumes them (else every prose edit rebuilds and re-grades the world), while `spec-lint`'s fileset includes the whole layer. Filesets give exactly this control; nothing needs inventing.

---

## 3. Hercules CI and Hydra: are requirements attributes?

Hercules CI's model: agents evaluate the flake hermetically, each attribute is a job, and "effects" carry the impure edge (deploys, secrets, network) *after* the pure jobs pass. Hydra is the same shape older: the jobset is the attribute set; CI status is per-attribute.

**The maximal reading** — every requirement ID yields a check derivation, coverage IS the attribute set, `nix flake check` IS the conformance bar — is seductive and must be priced honestly:

- *Eval cost*: fine. Even agency-scale (~1,500 criteria across 20 domains) is a few thousand attrs; Nix evaluates that in seconds-to-minutes, and nixpkgs proves the ceiling is far higher. Not the objection.
- *Build cost*: fine **because of caching**. An unchanged criterion over unchanged inputs is a cache hit; the second lane pays nothing. This is the economic argument *for* the design.
- *Sandbox limits*: the real boundary. No network, no agents, no forge probes, no secrets, no incremental state. A 4-hour non-hermetic Chromium fork build does not fit a check and never will; neither does anything touching `gh`, nor a warm-`target/` `cargo test` (the cold nix build of the same suite costs multiples). Tally's witnessed argv gates exist precisely for this territory and remain superior there: the witness produces a receipt (durable evidence with an actor and a timestamp), which a cached derivation does not.
- *The failure mode*: attributes that fake coverage — a check that greps for a string instead of exercising behavior. VD-5 was exactly this one layer down. Perturbation probes are the tax that keeps the design honest.

**The correct digestion** is not "requirements are attributes" but "**the requirement→oracle mapping is machine-checked, and attributes are one of three legal oracle forms**." Every `N.M` maps to exactly one of: (1) a check attribute (hermetic criteria — the majority for tally's own spec), (2) a witnessed gate argv named in the worklist (impure or incremental criteria), (3) an explicit `[HUMAN-ATTENDED]` mark. The lint computes this census and fails on any criterion in none of the three, or in two. That is Hercules' pure/effect split imported at the spec altitude, with tally's witness where effects sit. Coverage stops being a judgment and becomes an enumeration.

A useful composition falls out: since gates already run `nix build .#checks...<attrs>` (the epsilon-extension template does today), the derivation sitting can pre-digest each task's gate to build *the attributes of the criteria it cites* — the two-budget model from the D13 pilot, expressed as attr selection. The witness signs the receipt; Nix supplies reproducibility and the cache.

---

## 4. NixOS VM tests as executable acceptance

`pkgs.testers.runNixOSTest` is the closest thing in the ecosystem to Kiro's "derive tests from acceptance criteria," and tally already fields four of them (`stock-host-activation`, `system-socket-execution`, `retention-liveness-floor`, `flow-multi-host` — the last a **multi-node** test with a real Attic binary cache and a git remote between VMs). System-level EARS criteria — WHEN the daemon restarts, WHILE the poll holds the lock, SHALL CONTINUE TO reconcile old receipts — are exactly VM-test-shaped: full machine, real systemd, scripted events, assertions over observed behavior, hermetic.

**The convention, sketched.** `test/vm/<identity>/<req-id>.nix`, exposed as `checks.req-<identity>-N-M`; the testScript's structure mirrors the criterion (arrange the WHILE/WHERE state, fire the WHEN event, assert the SHALL); fixtures come from `specs/<identity>/contracts/`. The census (§3) closes the loop: a system-level criterion with no VM test and no witnessed gate is a lint failure, not a discovery.

**For agency this is THE oracle form.** A compositor/WM/shell's claims are almost all system-level, and a VM with a virtual display (headless wayland, virtio-gpu) is the only place they can be exercised hermetically. Two disciplines matter: prefer protocol-level byte assertions (wayland message captures against the frozen contracts) over pixel oracles wherever the spec allows — pixels flake, bytes don't, and agency's byte-level contracts were written for exactly this; and tier the gates — a lane's per-task gate builds only the VM tests for its cited criteria, the chapter gate builds the domain's full set, because 200 VM tests per lane is minutes-times-200 and does not fit.

**The substrate problem has a house answer already.** Agency's Chromium fork cannot be a per-lane derivation, but the VM tests need its binary. `flow-multi-host` already demonstrates the pattern: build on one host, `attic push`, `nix-store --realise` on the consumer. So: the substrate is built **once per substrate revision** by a witnessed gate (receipt, actor, duration — honest about its 4 hours and its impurity), pushed to the cache, and every VM test consumes the store path hermetically thereafter. The witness covers the impure act; the closure covers everything downstream.

---

## 5. Terraform plan / Kubernetes reconcile, at spec altitude

Tally is already a desired-state reconciler; K8s has little left to teach the campaign layer. The transferable idea is Terraform's **plan as a first-class rendered artifact**, applied to the one place the spec layer changes: between a ratified spec and a proposed amendment.

**Spec-diff as the boundary artifact.** When v2 is proposed against ratified v1 (pinned sha, §2), render — at requirement granularity, not line granularity — criteria added/removed/reworded, unchanged-behavior clauses touched, rulings superseded, and, crucially, **which tasks' epochs the derived amendment would refresh**. Ext0's `epoch-scoped-budgets` makes attempt counting a derivation from task bytes + gates + steering seq; the spec-diff can therefore *predict* budget refreshes the way `terraform plan` predicts resource churn, before the operator ratifies. That is the ratification review surface: not a git diff of prose, a rendered delta of law.

Terraform's hard-won lesson applies verbatim: **the plan must be computed by the same code path that applies**. A separate diff estimator drifts. The parser inside `spec-lint` (or the spec-build-driver, which already parses worklists from the spec tree) is the one parser; the diff is two parses and a join.

**Drift detection** maps onto what the house already does at stage boundaries: the edge census against the observed tree (F42, A12) *is* the drift check — the tree asserting things the spec's unchanged-behavior section doesn't know about. Worth making the census's output a committed artifact the sitting consumes, not worth making it a gate: drift at a boundary is information for the author, not a failure.

---

## 6. Selected extras (and two refusals)

**The golden-fixture double-pin, canonized.** `poolResourceDeclarationFixture` is the house's own best invention: one checked-in byte fixture; a producer-side check proving the fixture is *producible from the stated rules*; a consumer-side test on the other language reading the same bytes. This is precisely what the agency D13 pilot found missing ("fixture producibility" — the contract linter's second demand), discovered independently at tally scale. The house format should make it the required shape of everything in `specs/<identity>/contracts/`: no contract without a fixture, no fixture without a producer check and at least one consumer test. It converts the grind's expensive collision-detection (two blind derivations disagreeing) into a cheap, continuous, per-commit collision at the byte level.

**Perturbation probes as house law, mechanized.** `testBuildFailure'` makes "prove the gate can fail" a one-attribute cost. Every lint rule and every requirement-keyed check attribute of consequence gets a paired must-fail fixture. This is the D13 probe finding given a standing form.

**The modifying-delta budget (Igalia discipline) as a gate.** Trivially expressible today: a witnessed argv running `git diff --shortstat <stock-rev>..HEAD -- <substrate paths>` with the threshold read from the spec's contracts. The Nix-flavored variant — stock Chromium as a `flake = false` input pinned by narHash, delta computed in a derivation — buys reproducibility of the *baseline* at the cost of importing a 30M-line source tree into the store; honest verdict: commit the stock rev + narHash in `contracts/` as bytes, keep the diff a witnessed gate against a local checkout, and let the pin (not the sandbox) carry the authority. Agency-only; tally has no substrate.

**Property tests for quantified criteria.** An EARS criterion with "any"/"every" in its WHEN clause is universally quantified; a single example test under-discharges it. Convention: quantified criteria bind to `proptest` targets (criterion ID in the test name), and the lint's lexicon pass can flag a quantified criterion bound to an example-shaped oracle. Small, cheap, worth having.

**Two refusals, to avoid padding.** TLA+/formal specs: wrong altitude for this house — the contracts that would justify model-checking (agency's protocol state machines) are better served by the fixture double-pin plus VM tests, and a TLA+ artifact would be the canonical unconsumed artifact (no standing gate would execute it against the implementation). Content-addressing beyond git: `nix hash path` over spec dirs adds nothing the committed blob sha at the admitted revision doesn't already give under D77; refuted as redundant identity.

---

## 7. What Nix-specificity really means

Verdicts on the four candidates, then two additions.

**(a) The verification closure is a derivation — AFFIRMED, with the boundary priced.** For the hermetic subset of criteria, conformance is a pure function of committed bytes: reproducible, cacheable, remotely buildable, and — the campaign-economics point — *free on the second evaluation*. No script harness or IDE-bolted SDD has this: spec-kit re-runs its checks every time; a Nix check over unchanged inputs costs a hash lookup, which is what makes per-criterion granularity affordable at all. But tally's founding premise is "contention and proof for **impure** labor," and the boundary is real: witnessed argv gates are not a legacy to be migrated off, they are the permanent other half. Nix-native SDD means the split is explicit, enforced, and priced — not that everything becomes a derivation.

**(b) Identity is content-addressed — AFFIRMED.** Ratification pins a sha; the worklist names the spec sha it derives from; the receipt names the worklist sha (ext0 builds this); release renders the chain. The house already holds this doctrine for flake inputs; the spec layer is just the last unpinned link.

**(c) Hermeticity enforces byte-oracle-or-nothing — AFFIRMED, strongly.** This is the deepest one. In every other SDD harness, "this criterion has an executable oracle" is a discipline someone maintains. In the sandbox it is a **type distinction the toolchain enforces**: an oracle that can live in a check attribute is machine-checkable by construction (no network, no operator, no ambient state — it physically cannot be otherwise); an oracle that cannot is thereby *forced* into the witnessed or `[HUMAN-ATTENDED]` columns, visibly, in the census. The A10 law ("HUMAN-ATTENDED by declaration, not by discovery") stops depending on authorial honesty. The sandbox is the honesty.

**(d) "Which spec graded this code" is a store-path fact — AFFIRMED, with the toolchain included.** The frozen-flow rule already established that the deployed store path grades. Once spec-derived fixtures sit in the grading closure and the pin chain of (b) exists, attribution (A18) is a query over hashes. One under-stated corollary: **the toolchain is part of the conformance identity**. A clippy or rustc bump changes verdicts; `flake.lock` pins it; therefore a lock update is a grading-surface change and belongs in the same review class as a gate edit. The house format should say this once, explicitly.

**(e) Bisection turns conformance into evidence — my addition.** Because a check is a pure function of the tree, `git bisect run nix build .#checks.<system>.req-<id>` makes "when did criterion N.M break" a mechanical query. That is evidence *generation*, not just gating — receipts locate facts in time going forward; bisection over cached checks locates them backward. No non-hermetic harness can offer this, because a bisect through impure checks lies.

**(f) The fileset makes the anti-rot law decidable — my addition.** Nix is the only tool in the stack that forces explicit enumeration of what verification can see. Therefore "every artifact names its standing consumer" — currently a status-block prose field — can be a computed fact: an artifact under `specs/` that no check fileset and no gate argv names is provably unconsumed, and the lint deletes-or-fails. The grind's five-day silent rot becomes a build error the day it starts.

Summed in one sentence: **Nix-specificity means the spec layer's verification, identity, honesty-split, attribution, history, and rot-detection all become properties of the build graph instead of duties of the operator** — which is exactly what A8, the placement law, demands of any mechanism admitted to the house.

---

## 8. The mechanisms, ranked

### 1. `spec-lint`: the spec layer's standing consumer, as a flake check

**What it is.** One derivation, `checks.<system>.spec-lint`, that parses every `specs/<identity>/spec.md` against the format grammar (section order, status block fields, `N.M` ID uniqueness, EARS-class lexicon flags), resolves every cross-reference (worklist `goal` citations of requirement/evidence IDs, readFirst pointers, contract references — all against committed bytes only, which the sandbox enforces), renders the traceability table as an output (never checked in, per the doc derivation's own precedent), enumerates `specs/**` for consumer-less artifacts (mechanism f), and ships with `testBuildFailure'` fixtures proving each defect class fires. The parser is the same code the spec-build-driver and spec-diff use — one parser, per the Terraform lesson.

**Composes.** `runCommand` + `lib.fileset` over `self`; the spec-build-driver's parsing crate; `testers.testBuildFailure'`; the worklist JSON; the existing `mkCatalogRejectionCheck` idiom.
**Cost.** Writing and holding a markdown grammar rigid enough to parse without strangling the law prose; the parser is a real Rust deliverable (a few hundred lines in the driver crate); every future spec pays a formatting tax.
**Deletes.** The structural half of the manual analyze pass; the hand-maintained traceability table (a transcription act); the status-block "standing consumer" field as prose (it becomes computed); D68 pointer-checking by eye.
**Needed by.** Both, immediately. This is the artifact that keeps the spec layer alive under A15; without it the layer is the grind's bar relocated one level up.

### 2. The spec lock: ratification writes a pin, and the chain closes

**What it is.** Ratification stops being only a status-block edit and additionally records the ratified spec blob's sha256 (per `specs/<identity>/`, at the admitted revision) in machine-consumable form; the derived worklist carries that sha beside its existing `source.sha256`; ext0's receipt stamps carry it transitively via `worklistSha256`; `tally campaign release` renders which spec sha graded. The operator act is `nix flake lock`-shaped: a human runs a verb, the verb writes pins, everything downstream trusts pins.

**Composes.** The driver's existing `source.sha256`/`source.revision` emission; `receipt-authority-stamp` (ext0, already ratified); the release renderer; git blob addressing under D77.
**Cost.** Nearly nothing — one field through three schemas and one verb touch. The smallest mechanism here by an order of magnitude.
**Deletes.** Stale-pin archaeology at spec altitude (A18 becomes a lookup); "which version of the plan was this built against" as an operator recollection; the class of VD-13 twice-in-one-document attribution errors.
**Needed by.** Both, immediately. For agency — 20 domains, 2+ months, frozen specs — an unpinned spec-to-receipt chain would be untenable from day one.

### 3. The oracle census: every criterion maps to exactly one oracle form

**What it is.** A rule computed inside `spec-lint`: every `N.M` binds to exactly one of (i) a check attribute (`checks.req-<identity>-N-M`, frequently a `runNixOSTest` for system-level criteria), (ii) a witnessed gate argv named in the derived worklist, or (iii) an explicit `[HUMAN-ATTENDED]` mark. Zero bindings or two bindings fail the lint. The derivation sitting pre-digests each task's `flake-build-subset` gate to build the attributes of the criteria the task cites, so per-lane verification cost tracks the task, and the chapter gate builds the full set. Hermetic criteria get the cache; impure criteria get the witness; unfundable criteria get named, not discovered.

**Composes.** `spec-lint` (mechanism 1); the existing gate template (`nix build .#checks...` inside a witnessed argv); `runNixOSTest`; the attempt receipts.
**Cost.** Attribute sprawl and naming discipline; VM-test build minutes forcing honest gate tiering; the standing temptation of fake-coverage attributes, taxed by mandatory perturbation fixtures; eval time growing with criterion count (acceptable to agency scale, per §3).
**Deletes.** Hand-curated gate subsets; coverage doubt ("did anything exercise 4.1"); the [HUMAN-ATTENDED] honesty problem, which the sandbox now enforces (verdict c).
**Needed by.** Tally at moderate density (dozens of criteria, four VM tests already standing). **Agency at full density — this is its load-bearing mechanism**: compositor behavior is VM-oracle-shaped, and 20 domains of criteria without a computed census is unaudited by construction.

### 4. Contract fixtures with producer/consumer double-pins

**What it is.** `specs/<identity>/contracts/` holds byte oracles only in the canonized `poolResourceDeclarationFixture` shape: a checked-in fixture; a check proving the fixture is producible from the stated rules; a consumer test on every side that reads the contract, all reading the same bytes. The contract lint the D13 pilot demanded (cross-schema resolvability, fixture producibility) is these checks plus `spec-lint`'s reference resolution — not a new tool, a required shape.

**Composes.** The golden-fixture idiom already in the flake; `runCommand`; per-language test suites; `spec-lint` for the resolvability half.
**Cost.** Fixture maintenance across contract revisions (record-don't-fix applies: frozen fixtures are never edited to make progress); double-pinning means contract changes fail two suites at once, by design.
**Deletes.** Prose contract descriptions that two implementations read differently — the collision class the grind catches at the cost of two blind derivations, caught here per-commit for the byte-level subset. (The grind survives for semantic contracts; this narrows its load.)
**Needed by.** Tally lightly (a handful of Nix↔Rust pins, already practiced). **Agency at scale and non-negotiably** — its spec *is* byte-level contracts across 20 domains; this is the D13 linter finding given standing form.

### 5. Substrate-as-pinned-input with cache handoff — **agency only**

**What it is.** The stock Chromium baseline is committed as rev + narHash in `contracts/` (the pin carries the authority; the source tree stays out of the store). The fork substrate is built once per substrate revision by a witnessed gate — honest about being 4 hours and non-hermetic, receipted like any other impure act — and pushed to the binary cache (`flow-multi-host` already proves the Attic handoff end to end). VM tests and requirement checks consume the substrate as a store path, hermetically, forever after. The Igalia modifying-delta budget runs as a witnessed `git diff --shortstat <stock-rev>` gate with its threshold read from the spec.

**Composes.** Attic (exercised in-tree today); witnessed argv gates with receipts; narHash pinning; `runNixOSTest` consuming cached store paths; the delta gate as plain argv.
**Cost.** Cache infrastructure as a standing service; substrate-revision discipline (a stale substrate grading new lanes is the frozen-flow rule's failure mode — the receipt's substrate pin must join the epoch key); the one place where a multi-hour impure act sits permanently inside the campaign loop.
**Deletes.** Per-lane substrate rebuilds (which would make the campaign economically impossible — this mechanism is not an optimization, it is feasibility); "how large is our fork" as an estimate rather than a gated number.
**Needed by.** Agency only. Tally has no substrate; nothing here applies to it, and importing it early would violate A8.

---

**Rank rationale.** 1 and 2 are ordered by dependency, not importance — the lint is the layer's survival condition (A15), the lock is the cheapest link that closes the authority chain, and both are pre-agency work that epsilon-extension's own spec can prove. 3 is the mechanism that makes coverage a fact instead of a judgment and is where agency's weight lands. 4 is tally-optional, agency-mandatory. 5 is agency's feasibility condition and should not exist in tally's tree at all until the substrate does.

The one-line summary the house format should carry: **the spec layer is bolted to the flake the way the worklist is bolted to the poll — `spec-lint` is its standing consumer, the pin chain is its identity, the sandbox is its honesty, and every criterion names its oracle in a census a derivation computes.**
