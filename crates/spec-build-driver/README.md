# spec-build-driver

The Rust campaign driver implements the existing driver contract: one action
argument, a brief named by `TALLY_BRIEF`, and one `TALLY_FINAL_MESSAGE=` JSON
line on success.

All campaign actions run natively. This includes strict worklist witnessing
and campaign-policy validation, durable reconciliation and summary folds shared
with `tally-core`, steering and attempt receipts, checkpoint capture,
publication gates, conflict-domain enforcement, and linked-worktree merge and
cleanup mechanics.

## The `laneCapture` seam

The `retry` and `steer` briefs accept an optional `laneCapture` block —
`{adapter, adapterConfig, stdoutPath, stderrPath?, failureCode?}` — naming one
lane's retained capture and the adapter declarations to read it through. When
the adapter's declared `terminal` capture resolves, `src/adapter_outcome.rs`
classifies the lane `adapter-terminal`: the machinery retry is not bought and
the steering verdict settles to `blocked` without consulting the judge
(vestige-sweep V-16). Omitting the block classifies exactly as before, so the
key is additive.

**The flow does not populate it yet.** `examples/flows/spec-build.js` already
carries `capturePath` on failed nodes and forwards it for checkpoint tasks
only; forwarding it for agent-stage faults, beside the adapter the campaign
admitted under, is the flow-side edit that closes this seam and it lives
outside this crate.

The flow argument contract is typed in `src/flow_args.rs`. Its derived JSON
Schema is checked byte-for-byte against the pure-literal `meta.argsSchema` in
`examples/flows/spec-build.js`. After changing the Rust contract, regenerate
the flow with:

```console
cargo run -p spec-build-driver --example generate-flow-args-schema
```
