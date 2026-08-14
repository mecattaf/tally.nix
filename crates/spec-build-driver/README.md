# spec-build-driver

The Rust campaign driver is being ported one action at a time behind the
existing driver contract: one action argument, a brief named by `TALLY_BRIEF`,
and one `TALLY_FINAL_MESSAGE=` JSON line on success.

`prep`, `rebase`, and `cleanup` run natively, including linked-worktree
locking, identity, snapshots, base fetching, and branch management. The other
actions are dispatched to the Python driver while the port proceeds. Set
`SPEC_BUILD_PY_FALLBACK` to override that driver's executable path. The Nix
package compiles the packaged Python driver path in as the default, while a
workspace build defaults to the checked-out `drivers/spec_build_driver.py`.
