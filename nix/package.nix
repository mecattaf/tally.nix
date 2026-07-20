# nix/package.nix — the packaged tally binary (M0.1 scaffold owns this file).
#
# ONE Bun-compiled binary (TypeScript daemon + CLI, `bun build --compile`) packaged via
# bun2nix + autoPatchelfHook (SPEC "Flake outputs"; DECISIONS jul9 Bun flip). bun2nix's
# `mkDerivation` consumes the committed `bun.nix` (generated from `bun.lock` by
# `nix run .#bun2nix`) through `fetchBunDeps`, so the offline node_modules closure is
# reproducible.
#
# `pkgs` must carry the bun2nix overlay (flake.nix wires it), which supplies `pkgs.bun2nix`
# with `.mkDerivation` and `.fetchBunDeps`. Until `bun.lock` + a real `bun.nix` are generated
# (fresh checkout), a plain-stdenv placeholder builds the single-file entry directly with
# `bun`, so `nix build .#tally` yields a working `--help`-answering binary at layer 0 and
# `nix flake check` is green. The moment `bun install` + `nix run .#bun2nix` are run and
# `bun.nix`/`bun.lock` are committed, the reproducible bun2nix path takes over.
{ pkgs
, version ? "0.1.0"
,
}:
let
  inherit (pkgs) lib stdenvNoCC bun autoPatchelfHook;

  src = lib.cleanSourceWith {
    src = ../.;
    filter =
      path: type:
      let
        base = baseNameOf (toString path);
      in
        !(
          base == ".git"
          || base == "vendor"
          || base == "node_modules"
          || base == "dist"
          || base == "result"
          || lib.hasPrefix "result-" base
          || base == ".direnv"
          || lib.hasSuffix ".tsbuildinfo" base
        );
  };

  haveLock = builtins.pathExists ../bun.lock;
  haveBun2nix = pkgs ? bun2nix;

  # Reproducible build via bun2nix once the lockfile exists and the overlay is present.
  # `module = "src/main.ts"` drives bun2nix's default `--compile` flags; autoPatchelfHook
  # fixes the emitted ELF for the Nix loader.
  realDrv = pkgs.bun2nix.mkDerivation {
    pname = "tally";
    inherit version src;
    module = "src/main.ts";

    bunDeps = pkgs.bun2nix.fetchBunDeps {
      bunNix = ../bun.nix;
    };

    nativeBuildInputs = [ autoPatchelfHook ];

    meta = {
      description = "tally — agent-session orchestration (one Bun binary: daemon + CLI)";
      mainProgram = "tally";
      platforms = lib.platforms.linux;
    };
  };

  # Layer-0 placeholder: compiles the single-file entry with bun directly (scaffold's runtime
  # deps are near-zero), so the binary exists and runs before bun2nix codegen has been run.
  placeholderDrv = stdenvNoCC.mkDerivation {
    pname = "tally";
    inherit version src;
    nativeBuildInputs = [
      bun
      autoPatchelfHook
    ];
    dontConfigure = true;
    buildPhase = ''
      runHook preBuild
      export HOME=$TMPDIR
      bun build --compile --outfile=tally src/main.ts
      runHook postBuild
    '';
    installPhase = ''
      runHook preInstall
      mkdir -p $out/bin
      install -m555 tally $out/bin/tally
      runHook postInstall
    '';
    meta = {
      description = "tally — agent-session orchestration (one Bun binary: daemon + CLI)";
      mainProgram = "tally";
      platforms = lib.platforms.linux;
    };
  };
in
if (haveLock && haveBun2nix) then realDrv else placeholderDrv
