# Home Manager options ⚙️

> This page is generated from the evaluated Home Manager module by `nix build .#doc`.
> Do not edit the rendered page.

This is tally's complete deployed topology. In addition to the user daemon and witness
emitter, Home Manager renders the event drain, retention timer, producer units, usage-meter
units, and scheduled flow runners. Declaring any flow also supplies the reserved `flow` and
`build` pools with weak module defaults.
