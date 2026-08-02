# The GitHub login grammar, shared by every Nix-side gate on a reviewer entry.
#
# A reviewer login is rendered straight into an `@mention` on a public issue and
# into a GraphQL user lookup, so `crates/tally-core/src/producers/validate.rs`
# holds it to GitHub's own grammar at daemon config load: alphanumerics and
# interior hyphens, at most 39 characters. The module assertion has to enforce
# the *same* grammar, or a login the module accepts deploys green through
# `nixos-rebuild`/Home Manager and then refuses to start the daemon it was
# deployed for. `test/fixtures/gh-login/vectors.json` is the pinned corpus both
# sides run, so neither can drift alone.
let
  maxLength = 39;
in
{
  inherit maxLength;

  isValid =
    login:
    builtins.isString login
    && builtins.stringLength login <= maxLength
    && builtins.match "[A-Za-z0-9]([A-Za-z0-9]|-[A-Za-z0-9])*" login != null;
}
