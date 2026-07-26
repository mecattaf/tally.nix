# Pooled review cookbook

Keep the model roster in the consuming configuration and let tally own the catalog shape. The
`lib.tally.mkCatalog` helper evaluates every enabled member through typed Nix submodules, renders
the versioned catalog to the store, and validates that output with the real `tally flow check`
binary.

```nix
let
  reviewCatalog = tally.lib.tally.mkCatalog {
    inherit pkgs;

    classes.pooled-strongest.diversity = [
      "family"
      "maker"
    ];
    pools = [ "worker-gpu" ];

    members = {
      qwen-coder = {
        order = 10;
        family = "qwen";
        maker = "alibaba";
        classes = [ "pooled-strongest" ];
        adapter = "pi";
        pools = [ "worker-gpu" ];
        launch.model = "qwen-coder";
      };
      llama-review = {
        order = 20;
        family = "llama";
        maker = "meta";
        classes = [ "pooled-strongest" ];
        adapter = "pi";
        pools = [ "worker-gpu" ];
        launch.model = "llama-review";
      };
      mistral-review = {
        order = 30;
        family = "mistral";
        maker = "mistral";
        classes = [ "pooled-strongest" ];
        adapter = "pi";
        pools = [ "worker-gpu" ];
        launch.model = "mistral-review";
      };
    };
  };
in
{
  services.tally = {
    enable = true;

    pools.worker-gpu = {
      resource = "vram";
      capacity = 1;
    };

    flows.pooled-review = {
      script = ./flows/pooled-review.js;
      catalog = reviewCatalog;
      args = {
        subject = "the change under review";
        minimumValid = 2;
      };
    };
  };
}
```

The member attribute name becomes `id` by default. `order` controls the meaningful catalog array
order, with the attribute name as a deterministic tie-breaker. Set `enable = false` to retain a
roster row without emitting it. Class coverage is checked after that filtering, and the helper
fails evaluation with a named member, class, pool, or diversity key when the roster is not closed.
The optional `package` argument can select a custom tally package; otherwise the helper uses this
flake's package for `pkgs.stdenv.hostPlatform.system`.

The flow module performs the consuming half of the check. Because `catalog` is set,
`mkCheckedConfig` runs the script and rendered catalog together; every class in
`meta.selectors` must resolve to at least one member before activation can succeed.

Selectors resolve membership, not concurrency. With the capacity-1 pool above, the three members
drain sequentially and correctly. To buy real parallelism, declare a co-resident VRAM pool with
`capacity > 1` and a `budgetGb` partition that fits the host. The selector contract itself makes
no concurrency assumption.
