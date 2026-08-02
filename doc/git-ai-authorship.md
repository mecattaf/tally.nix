# Git AI authorship: one result, two authorities

Tally binds an admitted execution and its verdict to an exact result revision. Git AI binds that
revision to the agent sessions and edits that produced it. The integration is deliberately a
cross-link between those authorities, not a shared database or a second attribution engine.

| Concern | Authoritative owner | Integration rule |
|---|---|---|
| Scheduling, admission, pools, leases, retries, recovery | Tally | Git AI never influences resource arbitration. |
| Task, attempt, flow identity, and execution intent | Tally | Tally exports only bounded opaque correlation identifiers. |
| Process outcome, semantic gates, acceptance, evidence, witness chain | Tally | Authorship cannot make an invalid result valid. |
| Result repository and exact revision | Tally | The structured result declares the full Git object ID. |
| File-edit checkpoints and character or line attribution | Git AI | Tally does not reproduce them. |
| AI, human, and untracked classification | Git AI | Git AI remains the authorship authority. |
| Amend, rebase, cherry-pick, reset, stash, and merge migration | Git AI | Tally binds only the final revision and note. |
| Git notes, AI blame, authorship statistics | Git AI | Deep inspection stays in Git AI's CLI. |
| Agent Trace interoperability | Compatibility projection | Derive it from Git AI plus the Tally witness only for a concrete consumer. |

## Deployment boundary

`git-ai` is an environment-supplied fleet tool. Tom's dotfiles install it on every host where
Tally may execute a worktree. tally.nix does not package, vendor, pin, build, or fetch it, and the
Tally Nix modules expose no package option.

Enable the bridge independently of that provisioning:

```nix
services.tally.gitAi = {
  enable = true;
  mode = "advisory"; # or "required"
  awaitTimeoutSec = 60;
  globalAwaitOk = false;
};
```

When enabled, the executing host resolves `git-ai` from its configured environment. A missing or
unusable binary produces a reason beginning with `git-ai-unavailable:` that names the dotfiles
provisioning expectation. Required mode turns that provenance failure into a failed terminal
verdict. Advisory mode still runs the job and records `authorship.status = "unavailable"` without
changing an otherwise valid semantic verdict.

Remote execution follows the same rule: the worker that owns the worktree resolves and runs the
external tool. A coordinator-local installation is neither used nor sufficient.

## Execution and settlement

For each enabled attempt, Tally starts one private instance of the external Git AI daemon on the
worktree host. It gives that instance private control and Trace2 sockets and a private Git
AI-owned state directory. This isolates Git AI's process environment and custom attributes across
concurrent Tally jobs; it is not a Tally mirror of Git AI data.

The custom attributes contain only bounded join keys:

- task UUID;
- attempt and lease epoch;
- adapter;
- flow run ID and node ordinal when present.

Briefs, prompts, issue bodies, credentials, captures, paths to captures, and transcript payloads
are excluded.

After the child exits, Tally asks the private daemon to settle the result's repository family with
Git AI 1.6.17's `sync.family` control operation. Linked worktrees therefore share the correct
repository-family barrier, while an unrelated repository has a different daemon and cannot hold
up settlement. `globalAwaitOk` permits the external tool's global `await` interface only as an
explicit compatibility fallback inside this already isolated runtime. A successful barrier is
never treated as the proof: Tally next resolves the result revision and reads its note from
`refs/notes/ai`.

The terminal witness stores only:

- the full result revision;
- provider and observed provider version;
- authorship status and typed reason;
- `refs/notes/ai` and its exact target object ID;
- SHA-256 of the exact note blob bytes;
- up to 16 bounded, sorted correlated Git AI session observations (`tool`, `id`, and `model`).

It does not store the note body, prompts, transcript, edit ranges, or a Tally-generated
contribution manifest. A bound note also never overrides execution, gate, or acceptance truth.

## Query and independent verification

Query protocol 4 adds the compact `authorship` projection to both job and proof output:

```console
$ tally query job <task-uuid>
$ tally query proof --task <task-uuid> --attempt 2
```

The projection joins the canonical witness binding to the durable workspace identity. Tally's
session/model and Git AI's correlated sessions retain separate authority and provenance labels.
`identityMismatch` and the typed authorship reason expose disagreement; neither observation
silently replaces the other.

Verify the live repository against historical witness bytes without a Tally daemon or Git AI:

```console
$ tally witness verify-authorship \
    --ledger /var/lib/tally/witness.jsonl \
    --repository /path/to/worktree \
    --task <task-uuid> \
    --attempt 2 \
    --format json
```

The command first verifies the witness chain, selects the requested attempt and lease lane, then
uses Git plumbing to compare the revision, exact note bytes, and notes-ref target. Its status
distinguishes `match`, `revision-missing`, `missing-note`, `note-content-mismatch`,
`notes-ref-target-mismatch`, `not-bound`, an invalid ledger, and operational errors. It never
prints the note body. A later repository mutation can make this external check red while the
historical witness chain remains independently green.

The notes-ref target is checked by ancestry, not by equality. A notes ref is an ordinary commit
history that grows whenever any commit in the repository is annotated — including a campaign merge
node binding its squash commit — so equality would report a mismatch for every repository that
stayed in use. The witnessed target must still be an ancestor of the observed one; a ref that was
rewritten, rolled back, or rebuilt is not, and still reports `notes-ref-target-mismatch`. The proof
is unchanged and remains exact: the note blob for the witnessed revision must hash to the witnessed
digest.

For attribution details, follow the witnessed revision into Git AI:

```console
$ git ai show <revision>
$ git ai blame <path>
$ git ai stats
```

Tally intentionally has no prompt browser, line-range API, AI-blame implementation, or
authorship-statistics surface.

## Agent Trace relationship

Agent Trace is a possible lossy interoperability format, not another authority:

```text
Tally witness + result revision + Git AI authorship note
                        ↓
        optional Agent Trace compatibility document
```

The adapter boundary is field-by-field:

| Compatibility concept | Source of truth | Projection rule |
|---|---|---|
| Repository and exact revision | Tally workspace plus `resultRevision` | Emit the witnessed repository/revision pair; reject an unresolved or mismatched object. |
| Conversation or agent session | Git AI prompt/session record | Carry Git AI's stable session identifier and keep the Tally task UUID only as an external correlation key. |
| Model identity | Both, as separate observations | Preserve the requested/executing Tally model and the edit-producing Git AI model with their sources; never merge disagreement. |
| Files, character spans, and line ranges | Git AI authorship record | Project Git AI's recorded ranges exactly; never infer them from a final diff or Tally captures. |
| Prompt content | Git AI under its own retention policy | Reference or omit it according to the concrete consumer; never copy it into the Tally witness. |
| Execution, gates, and acceptance | Tally witness | Expose them only as Tally provenance extensions; they do not become authorship percentages or ranges. |
| Note integrity | Tally's note ref, target, and digest binding | Verify the live note before export and identify the historical witness separately from current repository state. |

No exporter is part of the core witness contract. If a concrete consumer later requires one, it
must derive sessions and ranges from Git AI and execution/result facts from Tally; it must not
reconstruct attribution from final diffs.
