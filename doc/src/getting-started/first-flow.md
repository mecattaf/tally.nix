# Run your first flow

This page runs the repository's real
`examples/flows/pooled-review.js` without requiring model credentials. A tiny
deterministic adapter stands in for three reviewers so the first run exercises
the actual catalog, pool, result-schema, quorum, repair, reducer, and witness
paths. Replace this adapter and roster with real agents after the mechanism is
working.

## Register the example

Add the following module after the module from
[Install tally](install.md). It keeps the `local` pool already declared there,
adds the pool used by the example's nodes, and registers a calendar-backed
flow whose timer is parked in 2099. We will trigger its generated service
manually.

```nix
({ pkgs, ... }:
let
  demoReviewer = pkgs.writeShellApplication {
    name = "tally-demo-reviewer";
    runtimeInputs = [ pkgs.jq ];
    text = ''
      prompt="$(jq -er .mission "$TALLY_BRIEF")"

      if [[ "$prompt" == Reduce\ these* || "$prompt" == Repair\ the\ reducer* ]]; then
        result='{"conclusions":[{"conclusion":"the demo path is wired","support":["deterministic-reviewer"],"conflict":[]}]}'
      else
        result='{"recommendation":"accept","evidence":["deterministic-reviewer"]}'
      fi

      jq -cn --arg result "$result" '{type:"result",result:$result}'
    '';
  };

  reviewCatalog = tally.lib.tally.mkCatalog {
    inherit pkgs;

    classes.pooled-strongest.diversity = [ "family" ];
    pools = [ "worker-gpu" ];

    members = {
      alpha = {
        family = "alpha";
        maker = "demo";
        classes = [ "pooled-strongest" ];
        adapter = "demo-reviewer";
        pools = [ "worker-gpu" ];
        launch.model = "alpha";
      };
      beta = {
        family = "beta";
        maker = "demo";
        classes = [ "pooled-strongest" ];
        adapter = "demo-reviewer";
        pools = [ "worker-gpu" ];
        launch.model = "beta";
      };
      gamma = {
        family = "gamma";
        maker = "demo";
        classes = [ "pooled-strongest" ];
        adapter = "demo-reviewer";
        pools = [ "worker-gpu" ];
        launch.model = "gamma";
      };
    };
  };
in
{
  services.tally = {
    pools.worker-gpu = {
      resource = "vram";
      capacity = 1;
      enforce = "cooperative";
    };

    adapters.demo-reviewer = tally.lib.adapters.mkAdapter {
      argv = [
        "${demoReviewer}/bin/tally-demo-reviewer"
        "--"
      ];
      launch.model = {
        argv = [ "--member" "%<value>%" ];
        allowedValues = [ "alpha" "beta" "gamma" ];
      };
      scrape.finalMessage = tally.lib.adapters.mkScrapeCapture {
        mode = "jsonPathLast";
        pattern = "$[?@.type == 'result'].result";
      };
    };

    flows.pooled-review = {
      script = tally.outPath + "/examples/flows/pooled-review.js";
      onCalendar = "2099-01-01 00:00:00";
      args = {
        subject = "the tally getting-started path";
        minimumValid = 2;
      };
      priority = "medium";
      dedupKey = "getting-started-pooled-review-%Y%m%dT%H%M%S";
      runtimeMaxSec = 120;
      evidence = [ "exit:0" ];
      maxNodes = 8;
      catalog = reviewCatalog;
      extraEnv.PATH = pkgs.lib.makeBinPath [
        tally.packages.${pkgs.system}.tally
      ];
    };
  };
})
```

Apply the Home Manager generation again:

```console
$ nix run github:nix-community/home-manager -- switch --flake .#alice
```

The checked configuration runs the production `tally flow check` against the
store-pinned script, arguments, and catalog before activation. Declaring a
flow also creates the reserved runner and build pools. The example's three
review nodes are submitted concurrently, but `worker-gpu` has one slot in this
walkthrough, so they execute one at a time. Selector count is membership, not
physical parallelism.

## Trigger, wait, and inspect

The flow registration rendered a calendar producer named
`flow-pooled-review`. Start its oneshot, drain the emitted event, and find the
new parent row by that producer origin:

```console
$ systemctl --user start tally-producer-flow-pooled-review.service
$ tally queue drain | jq .
$ flow_task="$(
    tally query jobs --origin flow-pooled-review |
      jq -er '.items | max_by(.timestamps.lastEventAt // "") | .taskUuid'
  )"
$ printf '%s\n' "$flow_task"
019…
```

Wait for the ordinary runner job. Its successful completion is itself
witnessed:

```console
$ tally queue await-job "$flow_task" | jq .
{
  "task_uuid": "019…",
  "verdict": "pass",
  "exit_code": 0,
  "witness_seq": 6,
  …
}
$ tally query proof --task "$flow_task" | jq .
```

Now inspect the node rows grouped by the runner's task UUID:

```console
$ tally query jobs --flow-run "$flow_task" |
    jq '[.items[] | {
      taskUuid,
      label: .orchestration.nodeLabel,
      pool,
      terminalVerdict,
      finalMessage
    }]'
```

You should see three review nodes and one dissent reducer, all with terminal
verdict `pass`. A later run may also contain repair nodes if a real reviewer
returns invalid structured output. Finish by verifying the whole ledger:

```console
$ tally witness verify --format json | jq .ok
true
```

## Which component owns the catalog contract

The current ownership is unambiguous: the JSON Schema lives in
`crates/tally-flow/schema/catalog.schema.json`, is embedded and enforced by
`crates/tally-flow/src/catalog.rs`, and is exercised by `tally flow check`.
`nix/lib/catalog.nix` renders typed catalog instances and validates them with
that binary; the `flow-catalog-schema` and `flow-catalog-renderer` flake checks
consume the same contract. Historical prose assigning schema ownership to a
later integration step is stale.

The flow producer rendering and its fixed runner pool are implemented in
`nix/modules/common.nix`. End-to-end runner, replay, catalog, and SSH behavior
is covered by `crates/tally/tests/flow_live.rs` and the
`flow-multi-host` flake check.
