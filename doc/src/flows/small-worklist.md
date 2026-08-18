# A small worklist, end to end

This page takes one ordinary repository — no spec plane, no frozen corpus, no
Nix in it at all — from nothing to a campaign that has run, been amended, been
steered, and rendered its release. It assumes the machine from [Install tally
as a product](../getting-started/product.md): a profile-installed CLI, a
running fleet-free daemon, and a green adapter smoke.

[Campaigns](campaigns.md) is the reference for every mechanism named below —
the manifest grammar, the ownership boundary, the merge criterion, the gate
ladder. This page is the short path through them, and it is deliberately the
lightest weight tally has: one committed JSON file is the entire campaign
surface.

The worked example is `acme/notes`, a small repository whose note validator
accepts front matter with no date.

## Scaffold the worklist

Run the verb inside the checkout that will arm it:

```console
$ cd ~/src/notes
$ tally campaign scaffold night-notes
{"worklist":"/home/alice/src/notes/silent-factory-worklists/night-notes.json","pattern":"silent-factory-worklists/night-notes.json","campaign":"night-notes","taskId":"night-notes","armArgv":["tally","campaign","arm","acme/notes","silent-factory-worklists/night-notes.json"]}

Wrote /home/alice/src/notes/silent-factory-worklists/night-notes.json

Campaign "night-notes", one implementation task "night-notes". Replace every value spelled EDIT-ME, and check:

  campaign.agent.adapter    an adapter this host's tally configuration declares
  campaign.gates            the commands that must pass before any task of this campaign merges
  tasks[0].conflictDomains  the paths this task is allowed to write

Commit the worklist to the base branch, then arm it:

    tally campaign arm acme/notes silent-factory-worklists/night-notes.json
```

The compact first line is the scriptable one; the human handoff follows after a
blank line, the way `campaign release --plan` already serves both consumers
from one rendering path.

Four things that line decided:

- **The identity names everything.** `night-notes` becomes the campaign name,
  the file `silent-factory-worklists/night-notes.json`, and the example task's
  id — derived down to the stricter lowercase-and-hyphen form a task id must
  have, and refused here if it cannot reach that form. `--path FILE` overrides
  the location; the file must still land inside the checkout that will arm it.
- **`acme/notes` was read from the checkout's own `origin` remote.** A local
  campaign's repository coordinate is an identity, not a fetch target — nothing
  contacts a forge with it. A checkout with no usable remote gets the
  placeholder `OWNER/REPO`, and the handoff says so: any stable identity will
  do.
- **The bytes were rehearsed before they were written.** The template is
  round-tripped through the same validation `arm` performs, so a scaffold that
  arm would refuse never reaches disk.
- **Nothing is overwritten.** Scaffolding an identity whose file already exists
  fails; an authored worklist is never clobbered.

## Edit the named placeholders

The template is minimal: what admission requires and nothing the heavyweight
genre adds on top of it — one gate, one implementation task, no spec plane and
no citation apparatus.

```json
{
  "schemaVersion": 1,
  "campaign": {
    "name": "night-notes",
    "agent": {
      "adapter": "claude-code"
    },
    "gates": [
      {
        "kind": "command",
        "id": "tests",
        "preflightArgv": [
          "sh",
          "-euc",
          "command -v bash >/dev/null"
        ],
        "argv": [
          "bash",
          "-lc",
          "echo 'EDIT-ME: replace this gate with the command every task of this campaign must pass before it merges' >&2; exit 1"
        ]
      }
    ]
  },
  "tasks": [
    {
      "id": "night-notes",
      "kind": "implementation",
      "title": "EDIT-ME: one line naming the change this task delivers",
      "goal": "EDIT-ME: state what the tree does today, the change this task makes, and the boundary it must not cross. The lane reads this and nothing else about your intent, so state the problem before the remedy and name what stays untouched",
      "deliveredBehaviors": [
        "EDIT-ME: one behaviour the tree has after this task that it does not have now"
      ],
      "readFirst": {
        "specSections": [
          "EDIT-ME: one file or note the lane must read before it changes code; an ordinary repository needs no spec plane here"
        ],
        "styleReferences": []
      },
      "acceptanceCriteria": [
        {
          "id": "tests-pass",
          "description": "EDIT-ME: one sentence saying what the command below proves",
          "argv": [
            "bash",
            "-lc",
            "echo 'EDIT-ME: replace this with the command that proves this task' >&2; exit 1"
          ]
        }
      ],
      "dependencies": [],
      "conflictDomains": [
        "src"
      ]
    }
  ]
}
```

`EDIT-ME` is the whole editing contract: one greppable marker, named by the
printed handoff, rather than a second list of fields to keep in sync with the
template. The two example argv are deliberately real commands that fail, so an
unedited scaffold cannot run green and the failure names the field to edit.

`readFirst.specSections` is non-empty because the validator requires a
non-empty list of strings there, not because a `specs/` tree has to exist.
Nothing resolves those strings against the filesystem: an ordinary repository
names an ordinary file, and this one names its own note-format note.

Edited, the same file reads:

```json
{
  "schemaVersion": 1,
  "campaign": {
    "name": "night-notes",
    "agent": {
      "adapter": "claude-code"
    },
    "gates": [
      {
        "kind": "command",
        "id": "tests",
        "preflightArgv": ["sh", "-euc", "test -x ./test/run.sh"],
        "argv": ["./test/run.sh"]
      }
    ]
  },
  "tasks": [
    {
      "id": "night-notes",
      "kind": "implementation",
      "title": "Reject a note whose front matter carries no date",
      "goal": "The note validator accepts any front matter that parses, so an undated note reaches the index and sorts arbitrarily. Make a missing or unparseable date a validation error with the offending path in the message, and leave the index writer alone: this task changes what validation rejects, not what the index does with what it accepts.",
      "deliveredBehaviors": [
        "validating a note with no date field exits nonzero and names the file"
      ],
      "readFirst": {
        "specSections": ["docs/note-format.md"],
        "styleReferences": ["src/validate.py"]
      },
      "acceptanceCriteria": [
        {
          "id": "tests-pass",
          "description": "The suite, including the new undated-note case, passes.",
          "argv": ["./test/run.sh"]
        }
      ],
      "dependencies": [],
      "conflictDomains": ["src/validate.py", "test"]
    }
  ]
}
```

The `goal` is the only thing the lane reads about your intent, so it states the
problem before the remedy and names what stays untouched. `conflictDomains` is
the enforced ownership boundary: the union of paths touched by every task
commit is compared against it immediately after the agent exits, before the
project gates run, and again against the clean head before anything is
published. A later deletion cannot hide a transient unowned path.

Before arming, confirm nothing was left behind:

```console
$ grep -c EDIT-ME silent-factory-worklists/night-notes.json
0
```

## Commit, push, arm

Arm reads the single regular file matching the pattern from the **fetched
remote base tree**. Dirty working-tree bytes are never campaign authority, so
the worklist has to be committed and pushed before it means anything.

```console
$ git add silent-factory-worklists/night-notes.json
$ git commit -m "arm the night-notes campaign"
$ git push origin main
$ tally campaign arm acme/notes silent-factory-worklists/night-notes.json | jq .
{
  "status": "armed",
  "issue": "local://acme/notes/silent-factory-worklists/night-notes.json",
  "codeRepository": "acme/notes",
  "worklistPattern": "silent-factory-worklists/night-notes.json",
  "tasks": 1,
  "graphDigest": "sha256:9166d806b11f199a8e4d4f95c86d05d23f171ebb1359494bf8185c3ed86c6683",
  "allowedActors": [
    "local"
  ],
  "enqueued": true,
  "warnings": [],
  "gateBudgets": [
    {
      "gateId": "tests",
      "runtimeMaxSec": 3600,
      "source": "unobserved",
      "observations": 0,
      "derivation": "gate tests: 3600s from the never-fired floor; no receipt records this gate firing"
    }
  ]
}
```

`--checkout` defaults to the current directory, `--base-branch` to `main`, and
`--remote` to `origin`. `--no-enqueue` registers and validates without admitting
the first reconcile pass; `--wait` blocks until that pass is terminal.

Read the rest of that receipt:

- **`graphDigest`** is the admitted graph. Every later pass compares the live
  worklist against it, and that comparison is what the next section is about.
- **`allowedActors: ["local"]`** — steering authorization is bound to the
  arming UID, not to a forge login.
- **`gateBudgets`** are derived, not declared. The worklist states no
  `runtimeMaxSec` for its gate, and the campaign has no receipt recording this
  gate firing, so the budget is the never-fired floor and the derivation says
  so in one sentence. Once the gate has run, later passes derive its budget
  from the campaign's own observations. A number in the worklist is a guess; a
  number from receipts is a measurement.
- **`warnings`** are advisory and arming continues past them. The common one
  names a path-shaped token in an acceptance or gate argv that falls outside
  the task's declared `conflictDomains` — worth reading, because it usually
  means the boundary is narrower than the work.

## Push to re-admit

After the first arm, **a worklist push is the arming act**. Amend the file,
commit, push:

```console
$ $EDITOR silent-factory-worklists/night-notes.json
$ git commit -am "night-notes: add the index-writer task"
$ git push origin main
```

There is no operator verb between the push and the dispatch. The poll timer
installed by the module scans the local registry every
`services.tally.campaignPoll.interval`, sees a graph digest that no longer
matches the admitted one, and admits the change as a fresh reconcile epoch.
Force one scan instead of waiting for the timer:

```console
$ tally campaign poll --once
```

The order is the safety property. Host validation runs against the pushed graph
*before* any authority moves, so a worklist naming an adapter this host cannot
serve is refused while the campaign is still armed on the epoch that worked: a
bad push cannot cost a good epoch. Only then do the receipt authority, the
approved-graph snapshot, and the registration advance together, and the
superseded snapshot stays on disk so an attempt that straddles the moment can
still be told which digest it owns.

`tally campaign arm` on an already-armed identity remains available and does
the same epoch flip with a human in it. Re-arming increments the retry
generation even when the graph did not change, which is how you ask for another
attempt at an unchanged worklist.

## Steer, and read the inbox

Steering is an ordered local log, appended by verb and snapshotted into the
next attempt's brief:

```console
$ tally campaign steer acme/notes silent-factory-worklists/night-notes.json \
    --task night-notes \
    --message 'The date field is ISO-8601 only; do not accept RFC 822.'
{"status":"recorded","codeRepository":"acme/notes","worklistPattern":"silent-factory-worklists/night-notes.json","taskId":"night-notes","sequence":1,"comment":{"id":1,"url":"local://campaign/01a0144c-2b58-7753-853b-78eb9275f761/steering/1","author":"uid:1000","body":"The date field is ISO-8601 only; do not accept RFC 822.","createdAt":"2026-08-18T09:56:14.699Z","updatedAt":"2026-08-18T09:56:14.699Z"},"doNotDispatchBefore":"2026-08-18T09:56:15.699Z","source":{"kind":"local-jsonl","path":"…/campaigns/steering/01a0144c-2b58-7753-853b-78eb9275f761/steering-v1.jsonl"},"offHostReceipt":"stdout-json-v1"}
```

`--task` must name an admitted task; omit it to steer the whole campaign. Use
`--message-file -` rather than `--message` when invoking the verb over SSH: the
stdin form does not expose or re-quote the text in remote argv.

The inbox is the other direction — the typed doubt the campaign is holding for
an operator, and which of it has already been answered:

```console
$ tally campaign inbox acme/notes silent-factory-worklists/night-notes.json
Inbox night-notes  0 entries, 0 open
```

Entries appear when a lane escalates rather than guesses: a task that needs an
authority surface outside its conflict domains, or one claiming the work as
specified is impossible. Each entry prints its sequence, kind, task, question,
any evidence paths, and the steering record that answered it; an entry with no
answer is open. Entries are facts and a re-admission does not retire them —
one that a new epoch quietly dropped would be one the operator was never told
about. `--json` emits the complete machine-readable projection.

Watch the pass itself with:

```console
$ tally campaign status acme/notes silent-factory-worklists/night-notes.json
$ tally campaign list
```

## Release

When the last task's work is merged and the campaign's lease lapses on a
published head, the campaign is complete and its release is renderable from
durable facts alone:

```console
$ tally campaign release acme/notes silent-factory-worklists/night-notes.json --plan
```

`--plan` contacts and changes no forge. It reads the campaign's own integration
ref, the gate-proof checkpoint, the merged task commits, and the closing
summary, then prints one compact JSON line followed by the human rendering:
the release plan and version, the campaign and its registration id, the
revision, the gate proof, one completion proof per implementation task, the
release notes, the artifacts (the integration ref, each task commit, each
checkpoint), and the campaign receipt. The version is derived, not chosen —
`0.0.0+<gate-proof commit timestamp>.<7-character revision>`.

Asking too early is a legible refusal rather than an empty document:

```console
$ tally campaign release acme/notes silent-factory-worklists/night-notes.json --plan
tally: completed campaign is missing local ref "refs/heads/tally/night-notes-campaign-01a0144c-2b58-7753-853b-78eb9275f761/integration"; restore its durable refs before rendering a release
```

`--probe` exercises the same release against a private disposable
`tally-probe-*` repository. With neither flag, the verb executes the release
through a `gh`-compatible forge program (`--gh-program` overrides the `gh` on
`PATH`), which is the one step of this page that needs a forge account.

## Put it away

```console
$ tally campaign disarm acme/notes silent-factory-worklists/night-notes.json
{"codeRepository":"acme/notes","worklistPattern":"silent-factory-worklists/night-notes.json","disarmed":true}
$ tally campaign quiescent
```

`disarm` removes only the locked local registration; the branches, refs, and
receipts the campaign produced stay where they are. `quiescent` prints nothing
and exits successfully only when the local registry is empty, which makes it
the condition to wait on before a reboot or an upgrade — otherwise it prints
the remaining registrations and exits nonzero.

## What this never needed

No spec corpus and no corpus freeze. No forge issue graph, and no `project`
step. No `services.tally.campaigns` declaration, no producer, no mention token,
and no label. No per-repository flow script or dispatch wrapper. One committed
JSON file, a daemon, and an adapter that passes its smoke.

Promote the campaign to [the declarative recurring surface](campaigns.md#configure-a-recurring-campaign)
the day the same worklist should be estate configuration rather than a file you
push. That is an explicit change of weight class, not a correction.
