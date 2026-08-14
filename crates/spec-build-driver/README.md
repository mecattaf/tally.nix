# spec-build-driver

The Rust campaign driver implements the existing driver contract: one action
argument, a brief named by `TALLY_BRIEF`, and one `TALLY_FINAL_MESSAGE=` JSON
line on success.

All campaign actions run natively. This includes strict worklist witnessing
and campaign-policy validation, durable reconciliation and summary folds shared
with `tally-core`, steering and attempt receipts, checkpoint capture,
publication gates, conflict-domain enforcement, and linked-worktree merge and
cleanup mechanics.

The flow argument contract is typed in `src/flow_args.rs`. Its derived JSON
Schema is checked byte-for-byte against the pure-literal `meta.argsSchema` in
`examples/flows/spec-build.js`. After changing the Rust contract, regenerate
the flow with:

```console
cargo run -p spec-build-driver --example generate-flow-args-schema
```
