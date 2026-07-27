# Run your first job

The `local` pool from the previous page is enough to run and witness one job.
This example asks for three independent facts: exit code zero, a regular
artifact at an absolute path, and a recorded SHA-256 hash of that artifact.

```console
$ artifact="${XDG_STATE_HOME:-$HOME/.local/state}/tally/hello.txt"
$ mkdir -p "$(dirname "$artifact")"
$ result="$(
    tally enqueue \
      --pool local \
      --priority medium \
      --adapter shell \
      --runtime-max-sec 30 \
      --evidence exit:0 \
      --evidence "artifact:$artifact" \
      --evidence hash:sha256 \
      --wait \
      -- sh -c 'printf "%s\n" "hello from tally" > "$1"' tally "$artifact"
  )"
$ printf '%s\n' "$result" | jq .
{
  "artifact_content_hash": "sha256:…",
  "attempt": 1,
  "exit_code": 0,
  "job_id": "…",
  "lease_epoch": 1,
  "task_uuid": "…",
  "verdict": "pass",
  "witness_seq": 1
}
```

tally did not interpret the shell program. `sh`, `-c`, the program text, the
sentinel `tally`, and the artifact path were five explicit argv elements. If you
do not ask for a shell, tally never inserts one.

`--wait` returns only after the terminal witness is durable. Save its stable
task UUID and inspect the joined row, lifecycle observations, evidence, and
canonical result:

```console
$ task="$(printf '%s\n' "$result" | jq -r .task_uuid)"
$ tally query job "$task" | jq .
$ tally query proof --task "$task" | jq .
```

Finally, verify the complete canonical and advisory chains from disk:

```console
$ tally witness verify --format json | jq '{ok, chains}'
{
  "ok": true,
  "chains": {
    "attestations": { "report": { "ok": true, … }, … },
    "verdict": { "report": { "ok": true, … }, … }
  }
}
```

The exact sequence and hashes will differ on your machine. The important
result is `verdict: "pass"`, a nonempty `artifact_content_hash`, and
`ok: true`.

This path is implemented by the public CLI in `crates/tally/src/main.rs`, the
durable enqueue and wait boundary in `crates/tally-core/src/daemon.rs`, direct
systemd argv construction in `crates/tally-core/src/executor.rs`, and the
evidence gate in `crates/tally-core/src/evidence.rs`. The
`crates/tally/tests/cli_rpc.rs` integration test exercises the same CLI/RPC
boundary.
