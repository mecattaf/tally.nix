# Evidence and gates

tally keeps three questions separate:

1. **Execution:** did the process run successfully?
2. **Gates:** what did the declared semantic checks report?
3. **Acceptance:** does the selected policy accept those facts?

The canonical witness verdict is grounded in execution and evidence. Gate facts
may downgrade an otherwise passing verdict, but an acceptance decision is
recorded as its own fact rather than rewriting history.

## Evidence checks

An enqueue accepts four evidence forms:

| Form | What tally proves |
|---|---|
| `exit:N` | the recorded process exit code equals `N` |
| `artifact:/absolute/path` | the target is a stable regular file and can be hashed |
| `hash:sha256` | a SHA-256 content hash was produced for declared artifacts |
| `hash:sha256:HEX` | the produced aggregate artifact hash equals `HEX` |
| `store:/nix/store/…` | Nix reports the canonical store path as valid |

Multiple artifact hashes are combined in declaration order into one recorded
SHA-256 value. Store paths are verified, sorted, and recorded separately.
Artifact hashing checks the file identity and metadata before and after reading
so a concurrent replacement or mutation fails the check.

Every run also records a finite, non-negative witness span. A clean exit with a
missing, changed, invalid, or hash-mismatched artifact/store requirement
becomes `clean-exit-no-artifact`. A wrong exit or invalid span becomes
`failed`. This distinction is useful: “the command ran” and “the command
produced the claimed result” are different facts.

Evidence parsing and evaluation live in
`crates/tally-core/src/evidence.rs`. Its
`hashing_fails_closed_when_the_open_artifact_changes_or_is_replaced`,
`store_gate_checks_each_path_once_sorts_passes_and_fails_closed`, and
`store_only_dedup_revalidates_and_requires_witness_set_equality` tests pin the
filesystem and Nix-store behavior.

## Gate manifests

A gate manifest specification names an absolute JSON path, a set of required
gate IDs, and an acceptance policy. tally exports the effective path as
`TALLY_GATE_MANIFEST` so the workload can write the manifest. If no gate
contract applies, the executor explicitly scrubs that environment variable.

The manifest itself contains `schemaVersion`, an opaque `artifact` value, and
an array of gates. Each gate has an ID and status `pass`, `fail`, or `not-run`;
a not-run gate must say why.

The current absent-manifest rule is precise:

- a declared path that does not exist produces gate summary `not-run`, not
  `fail`;
- empty `requiredGateIds` is valid;
- the `codex` and `claude-code` presets synthesize a per-attempt manifest
  specification with empty required IDs and manual acceptance when the job did
  not declare one;
- adapters outside those two presets do not gain an implicit gate contract.

Consequently, a preset job that exits and satisfies evidence but writes no
manifest keeps its evidence verdict. Its semantic completion says gates
`not-run` and acceptance `pending`; tally does not dress that absence up as a
pass and does not mutate the row to invent requirements.

An existing malformed manifest is different: it produces gate summary `fail`.
A manifest that omits a nonempty required ID or contains a gate marked `fail`
also fails the summary. If evidence would otherwise pass, execution failure or
gate summary `fail` downgrades the canonical verdict to `failed`. A well-formed
explicit `not-run` remains `not-run`.

With `execution-and-gates` acceptance, successful execution plus gate summary
`pass` is accepted, a failure is rejected, and `not-run` remains pending.
Manual acceptance remains pending in this implementation; the completion fact
does not claim that a human action occurred.

These rules are implemented in `crates/tally-core/src/completion.rs`,
`crates/tally-core/src/daemon.rs`, and the executor's effective-manifest logic.
Tests `absent_manifest_is_visible_not_run_without_failing_execution`,
`zero_exit_with_failed_and_missing_declared_gates_is_semantically_rejected`,
and `gate_manifest_path_is_exported_or_scrubbed_and_defaults_per_target` pin
the historical divergence and its execution boundary.

Inspect evidence, semantic completion, and canonical authority in one joined
projection:

```console
$ tally query proof --task "$task" | jq '{evidence, completion, canonical}'
```
