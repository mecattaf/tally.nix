# spec-build-driver

The Rust campaign driver is being ported one action at a time behind the
existing driver contract: one action argument, a brief named by `TALLY_BRIEF`,
and one `TALLY_FINAL_MESSAGE=` JSON line on success.

`worklist`, `sweep`, `reconcile`, `diff`, `prep`, `rebase`, and `cleanup` run
natively. This includes strict worklist witnessing and campaign-policy
validation, durable reconciliation and summary folds shared with `tally-core`,
dead-lane sweeping, diff capture, and linked-worktree mechanics. The remaining
actions are dispatched to the Python driver while the port proceeds. Set
`SPEC_BUILD_PY_FALLBACK` to override that driver's executable path. The Nix
package compiles the packaged Python driver path in as the default, while a
workspace build defaults to the checked-out `drivers/spec_build_driver.py`.

The flow argument contract is typed in `src/flow_args.rs`. Its derived JSON
Schema is checked byte-for-byte against the pure-literal `meta.argsSchema` in
`examples/flows/spec-build.js`. After changing the Rust contract, regenerate
the flow with:

```console
cargo run -p spec-build-driver --example generate-flow-args-schema
```
