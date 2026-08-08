# Final conformance bar: Phase 3 validation

Date: 2026-08-08

The validated suite revision is `247d3d4`. Its production baseline is
`main` at `4a99f64`; every path changed after that baseline is a document,
test, fixture, harness, or the additive flake check. No production source was
changed.

The definitive run was:

```console
python3 test/final-bar/run.py . --artifacts /tmp/fb-phase3-final
```

It completed all 26 cases with **3 PASS, 23 FAIL, and 0 ERROR**. The three
passes were `git-ai-task-ref-contract`,
`git-ai-validation-terminal-cause`, and
`launch-cwd-restarted-capture`. A FAIL is the expected product verdict for an
unresolved desired-state assertion; zero ERRORs establishes that the harness
itself built its target artifacts and ran through the last case.

The measured #419 population case ran the exact prebuilt `tally_core` binary
for 480 seconds in three concurrent lanes at default test parallelism. It
observed 1 failing run out of 15. The retained panic was
`daemon::tests::preset_gate_defaults_distinguish_absent_manifest_from_gates_passed`
at `daemon/tests.rs:6909`, where a wall-clock deadline produced
`Elapsed(())`. This is one of the four unresolved mechanisms named by #419,
not a new harness failure.

## Failure matrix

| Case | Issue(s) | Intended behavior pinned | Current-HEAD evidence |
|---|---|---|---|
| `adapter-argv-corpus` | #443, #445 | The evaluated Codex preset declares session-cumulative usage, resumes a real no-model stream without `--model`, and the Pi preset refuses option-looking workload heads; the Rust renderer must consume those declarations exactly. | Codex still requires `%<model>%`, omits `counterScope`, and cannot resume the recorded no-model stream; Pi declares no refusal policy and renders both forbidden workloads. |
| `campaign-config-locator` | #442 | Initial and continuation campaign children serialize the admitting host's exact `--config <path>` prefix and observe the same non-default sentinels. | The initial packaged child omits the configured path, so the end-to-end locator contract fails before continuation can prove it. |
| `campaign-full-pipeline` | #439, #441, #442, #444, #446, #448 | One packaged run crosses arm, poll, reconcile, dispatch, execute, sweep, and digest while preserving manifest/domain semantics, the seven Git-AI correlation fields, exact child config, strict validation, and owned immutable assets. It merges the task result and emits one closing digest. | Reconcile is nonzero, the child omits its config locator, no `owned-paths-fallback` reaches durable surfaces, no result is merged, and no terminal close/digest is emitted. This combined case stops before all otherwise-standalone downstream guarantees can be exercised. |
| `campaign-manifest-corpus` | #439, #444, #446 | Rust arm and the packaged Python driver accept and canonicalize the same valid bodies to identical bytes/digests, distinguish omitted from explicit conflict domains, and reject every shared closed-schema, path, regex, nullability, and model-boundary fixture before side effects. | Valid arm digests disagree with the normative driver contract, and Rust accepts multiple fixtures the driver correctly rejects and registers them as armed. |
| `campaign-nonstore-snapshots` | #448 | Non-store flow/driver overrides become immutable registration-owned executable snapshots rather than live mutable paths. | Authority and both executable references still point into the mutable override directory. |
| `campaign-registry-forward-read` | #447 | The current reader accepts a literal N-1 v2 authority, supplies the 10-second host-tuning default, and does not rewrite authority. | The forward read supplies no default tuning value. |
| `campaign-registry-n-minus-one` | #447 | Current default and explicit-tuning registrations keep closed v2 authority readable by the actual N-1 binary; tuning lives in an ignored sidecar, and the current reader also accepts N-1 output. | `projectionWaitMs` leaks into authority, N-1 rejects it as unknown, and the explicit sidecar is absent. |
| `campaign-store-asset-ownership` | #448 | Flow and driver store objects each receive an independent indirect root before registration visibility, survive collection/upgrades, execute at the exact recorded versions, and are cleaned on disarm/prune. | The root recorder sees no ownership calls for either store object. |
| `codex-model-recovery` | #443 | A real default-model capture resumes without a model flag, while an explicitly admitted model is pinned exactly once through continuation/recovery. | The default path cannot reach a resumed process from its scraped session, so the two required argv forms are not established. |
| `eval-manifest-zero-covered` | #426 | Schema-valid manifests whose declared surfaces are all accounted for but none are directly covered return exit 4 with `verification=none`; mixed direct coverage returns 0 with `verification=present`, with documented multi-file precedence. | Zero-covered fixtures return 0, stable verification tokens and contract documentation are absent, and mixed multi-file precedence collapses to 0. |
| `launch-cwd-adopted-metadata` | #440 | A running continuation adopted at startup retains its exact launch cwd, and a focused pre-fallback regression observes the adopted-metadata producer binding. | The focused producer regression is absent. |
| `launch-cwd-ordinary-completion` | #440 | Completion-time session scraping records `sessionRef` and launch cwd together; public same-cwd continuation succeeds and a focused regression observes the row before fallback hydration. | The full run could not continue from a scraped session, and the focused audit separately confirmed that the producer regression is absent. |
| `launch-cwd-recovered-install` | #440 | A paused job installed from recovery retains the exact launch cwd, with both public continuation and focused pre-fallback producer coverage. | The focused recovered-install regression is absent. |
| `launch-cwd-representation-hydration` | #440 | Startup re-presentation hydrates the exact launch cwd beside the session pointer, with a public recovery consequence and a focused pre-fallback producer regression. | The focused re-presentation regression is absent. |
| `parallel-causal-regressions` | #419 | Each of the four named deadline cases uses a counted causal event or explicit quiescence barrier and passes independently; a bare wall-clock `Elapsed` unwrap is not a repair. | All four target bodies still contain bare `tokio::time::timeout(...).await.unwrap()` paths (eight sites in total), although isolated executions happen to pass. |
| `parallel-population-wave` | #419 | The exact candidate test binary runs for 480 seconds in three concurrent suites at default parallelism with zero failing full-suite runs. | Default-parallel invocation was verified, then 1 of 15 full-suite runs failed in the named preset-gate deadline test with `Elapsed(())`. |
| `pi-prelaunch-refusal` | #445 | Pi workloads `--version` and `-p` receive a typed `option-like-workload-head` refusal before enqueue/process creation; normal workloads remain launchable. | Both forbidden values execute successfully and the process-spawn sentinel is touched. |
| `reader-state-explicit-identity` | #415 | Explicit run/job identity lookup returns archived durable members, while combining explicit identity with `--no-archived` is rejected as contradictory. | Archived members are hidden and the contradictory request is silently accepted. |
| `reader-state-view-aggregates` | #415 | Default browse filtering defines a view: aggregates use visible rows only, while hidden-row counts remain explicit. | GPU time from the hidden archived row leaks into the visible aggregate. |
| `usage-attempt-census` | #402 | A durable attempt counter of 3 with only attempt 3 attested reports three expected, two missing, the stable caveat, and incomplete usage. | The attempt denominator/missing fields and caveat are absent, and no `isComplete: false` verdict is projected. |
| `usage-codex-cumulative-delta` | #403 | The recorded fresh/resumed Codex checkpoints contribute the fresh zero-baseline plus one verified delta; a cumulative reading without its predecessor is caveated, contributes nothing, and is incomplete. | Raw cumulative snapshots are added (double-charging the first attempt), and a missing-baseline checkpoint is charged and graded complete. |
| `usage-declared-surfaces` | #408 | Completeness is graded per dispatch-time declared field across two-field, cost-only, total-only, and components-plus-total-drift adapters, with field census and drift caveats. | Declared/reported field census is absent, universal legacy inference still fires, and component drift is not named or made incomplete. |
| `usage-legacy-total-only` | #409 | Legacy records are graded only from reported shape; multiple total-only attempts retain `total-only-attempts` and `declared-surface-unknown`, and public wording states the limitation. | The declared-surface caveat and the required legacy/reported-shape contract wording are absent. |

## Audit of the nine original passes

Every Phase 2 pass has exactly one of the required resolutions below.

| Original passing case | Resolution | Evidence |
|---|---|---|
| `git-ai-task-ref-contract` | **Already fixed on HEAD.** | Base commit `4a99f64` adds `taskRef` to both the seven-field producer and the validator's closed allow-list. The public daemon/process probe receives exactly the seven keys and the intended values. Removing the validator allowance was caught by the mutation run below. |
| `git-ai-validation-terminal-cause` | **Already fixed on HEAD.** | Base commit `4a99f64` adds a canonical structured executor-validation error to waiter, witness, and `query.run`, plus a flow regression that skips advisory projection waiting. Both exact target regressions passed in the full run; their assertions inspect those surfaces and the absence of a capture path/poll. |
| `launch-cwd-adopted-metadata` | **Test repaired in `247d3d4`.** | The public lookup can re-derive `session_cwd` after this producer, masking deletion. The case now also requires `adopted_metadata_records_launch_cwd_beside_the_pointer`; HEAD omits it and the case fails. |
| `launch-cwd-ordinary-completion` | **Test repaired in `247d3d4`.** | Deleting the completion binding at `completion.rs:463` left the old public case green because downstream lookup re-hydrated the field. The case now also requires `completion_scrape_records_launch_cwd_beside_the_pointer`; HEAD omits it. |
| `launch-cwd-recovered-install` | **Test repaired in `247d3d4`.** | The case now pairs its public recovery scenario with `recovered_job_install_records_launch_cwd_beside_the_pointer`; HEAD omits the focused producer assertion. |
| `launch-cwd-representation-hydration` | **Test repaired in `247d3d4`.** | The former combination proved re-presentation and the already-bound restart seam, but not this producer. It now requires `represent_hydration_records_launch_cwd_beside_the_pointer`; HEAD omits it. |
| `launch-cwd-restarted-capture` | **Already fixed on HEAD.** | `daemon::tests::a_restarted_daemon_re_derives_the_launch_record_beside_the_pointer` observes `session_ref` and exact `RecordedLaunchCwd` in the recovered row before public continuation. The full case passes, and deleting its startup binding made that assertion fail `None != Some(In(...))`. |
| `parallel-causal-regressions` | **Test repaired in `247d3d4`.** | Running the four tests in isolation only repeated the issue's known false reassurance. The repaired case rejects bare deadline unwraps in each named body before also executing the exact tests; all four unresolved bodies now fail the case. |
| `parallel-population-wave` | **Test repaired in `247d3d4`.** | The 480-second gate already parsed its measured total, but did not bind the requirement that the target binary remain at default parallelism. A sentinel now audits argv and `RUST_TEST_THREADS` before the real wave. The repaired full run also caught 1/15 failing suites. |

The #440 audit exposed a genuine specification defect rather than a design
choice to revisit: `session_cwd` is not serialized, and the public lookup
deliberately re-derives it from a retired row. Therefore a public continuation
cannot distinguish a missing producer binding from downstream fallback. The
specification's §7.2 and §9 row now require both the public consequence and a
focused regression that observes the producer row before fallback. That
clarification and the repaired cases are in `247d3d4`.

## Mutation spot checks

All mutations were made in an isolated temporary worktree, tested through the
suite entry point, reverted, and then removed along with the mutated binaries.

| Deliberate wrong behavior | Conformance case | Result | Evidence |
|---|---|---|---|
| Add `--test-threads=1` to every `flake-probe.sh` test-binary invocation (#419). | `parallel-population-wave` | **CAUGHT** | Failed in 0.95s before the long wave; the sentinel recorded repeated `argv: ["--test-threads=1"]`. |
| Delete the restart-path `record_session_launch_cwd()` binding in `apply_adapter_metadata` (#440). | `launch-cwd-restarted-capture` | **CAUGHT** | The focused target regression failed with recovered `session_cwd` `None` instead of `Some(In(<session-home>))`. |
| Remove `taskRef` from `GitAiExecution::validate`'s closed correlation allow-list (#441). | `git-ai-task-ref-contract` | **CAUGHT** | The public task-ref payload became a nonzero terminal verdict; its canonical witness named `executor-validation-failed` and the bounded-correlation-set rejection. |

There were no missed spot mutations. The final branch contains no mutation
source, worktree, or mutated binary residue.
