# spec-build-driver

The Rust campaign driver is being ported one action at a time behind the
existing driver contract: one action argument, a brief named by `TALLY_BRIEF`,
and one `TALLY_FINAL_MESSAGE=` JSON line on success.

For now every action is dispatched to the Python driver. Set
`SPEC_BUILD_PY_FALLBACK` to override that driver's executable path. The Nix
package compiles the packaged Python driver path in as the default, while a
workspace build defaults to the checked-out `drivers/spec_build_driver.py`.
