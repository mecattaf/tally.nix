# NixOS options ⚙️

> This page is generated from the evaluated NixOS module by `nix build .#doc`.
> Do not edit the rendered page.

The NixOS wrapper deploys the system daemon and witness emitter. It accepts the same typed
`services.tally.*` tree as Home Manager, but that does **not** make the topology equivalent:
it emits no producer, usage-meter, retention, event-drain, or scheduled-flow units, and it
does not auto-declare the `flow` or `build` pools. Use Home Manager for those mechanisms.
