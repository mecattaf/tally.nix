# nix/dev.nix — the tally dev rig (M3.4 dev-rig). OVERWRITES the layer-0 scaffold placeholder
# (the trivial no-op `apps.dev` in flake.nix) with the real process-compose rig — the
# "scaffold creates, dev-rig overwrites" handoff (M0.1/M3.4).
#
# This is a FLAKE-PARTS MODULE. The integrator wires it into flake.nix by:
#   1. adding `inputs.process-compose-flake.flakeModule` to the top-level `imports`;
#   2. adding `./nix/dev.nix` to the top-level `imports`;
#   3. deleting the inline `devApp` placeholder + its `apps.dev` binding from flake.nix
#      (this module exports `apps.dev` itself, pointing at the process-compose-generated
#      `packages.dev` — two `apps.dev` writers would collide).
# See the SEAM NOTE returned to the integrator.
#
# `nix run .#dev` → `process-compose up` booting the daemon against MOCK jobs on a laptop with no
# GPU and no worker box (SPEC "Inputs & dev rig"; BUILD-SEQUENCE step 1 acceptance). Production
# stays systemd user units (M3.3). A process named `test` becomes the flake check `checks.dev`
# (derivation `dev-test`, a process-compose-flake convention): it waits for the daemon healthy,
# then runs the smoke script asserting `session.snapshot` answers and one mock job completes.
#
# Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).
{ ... }:
{
  perSystem =
    { self', pkgs, lib, ... }:
    let
      # The one Bun binary (daemon + CLI). `self'.packages.tally` is scaffold's package output.
      tally = self'.packages.tally;

      # The mock substrate, carried into the store so the rig is hermetic (no reliance on the
      # checkout being writable or on a particular cwd). Kept as a filtered source tree rather than
      # individual `writeText`s so the scripts keep their relative `events-samples/` layout. The scripts'
      # `#!/usr/bin/env bash` shebangs are patched to the store bash because the `nix flake check`
      # sandbox has no `/usr/bin/env` — an unpatched fake-worker fails with "bad interpreter" when the
      # jobs engine (or the smoke fallback) execs it as a leaf.
      mockSrcRaw = lib.cleanSourceWith {
        src = ../dev;
        filter =
          path: type:
          let base = baseNameOf (toString path); in
            !(base == ".rig");
      };
      mockSrc = pkgs.runCommandLocal "tally-dev-mock" { nativeBuildInputs = [ pkgs.bash ]; } ''
        cp -r ${mockSrcRaw} "$out"
        chmod -R u+w "$out"
        patchShebangs "$out/mock"
      '';

      # The MOCK pls broker (the "laptop with no GPU" path): tally acquires a pls lease before every
      # heavy unit, so the rig needs a `pls` on PATH that always grants. dev/mock/pls.sh models the
      # documented surface; we expose it as `pls` so the broker's `PLS_BIN` resolves it from PATH.
      mockPls = pkgs.writeShellApplication {
        name = "pls";
        runtimeInputs = [ pkgs.bash pkgs.coreutils ];
        text = ''exec bash ${mockSrc}/mock/pls.sh "$@"'';
      };

      # Runtime deps the mock scripts and the daemon lean on. No GPU/worker/pls — the rig is the
      # no-hardware path; `socat` is optional (the scripts fall back to python3), included for a
      # snappier RPC. `bash`, `coreutils`, `gawk`, `python3` are the script's hard deps.
      rigPath = lib.makeBinPath [
        tally
        pkgs.bash
        pkgs.coreutils
        pkgs.gawk
        pkgs.python3
        pkgs.socat
      ];

      # The shared-env prologue every rig process sources: it points XDG_STATE/DATA/CONFIG at ONE
      # scratch tree under the runtime dir so the rig never touches the operator's real ~/.local
      # state, and resolves a writable runtime dir for the socket (sandbox-safe). Sourced by both
      # the daemon and the enqueue/test drivers so they agree on the same tree.
      rigEnv = ''
        export PATH=${rigPath}''${PATH:+:$PATH}
        # Resolve a WRITABLE runtime dir for the Unix socket. `nix run .#dev` has the real
        # $XDG_RUNTIME_DIR (/run/user/UID); the `nix flake check` sandbox has none (and /run/user is
        # unwritable there), so fall back to a short per-user tmp dir — a Unix socket path must stay
        # well under the ~108-char sun_path limit, so we keep the base short.
        if [ -z "''${XDG_RUNTIME_DIR:-}" ] || [ ! -w "''${XDG_RUNTIME_DIR:-/nonexistent}" ]; then
          XDG_RUNTIME_DIR="''${TMPDIR:-/tmp}/tally-rig-$(id -u)"
        fi
        export XDG_RUNTIME_DIR
        # ONE shared scratch tree for the whole rig, keyed off the runtime dir so the daemon and the
        # enqueue/test processes agree on the same state/data/config (the witness ledger, events/
        # drop dir, and artifacts must be co-located once the jobs engine is composed). Deterministic
        # per runtime dir — NOT a per-process mktemp — so both processes see one tree.
        # Override with $TALLY_DEV_RIG_ROOT.
        RIG_ROOT="''${TALLY_DEV_RIG_ROOT:-$XDG_RUNTIME_DIR/tally/dev-rig}"
        export XDG_STATE_HOME="$RIG_ROOT/state"
        export XDG_DATA_HOME="$RIG_ROOT/data"
        export XDG_CONFIG_HOME="$RIG_ROOT/config"
        # taskwarrior data + rc under the rig tree: a `source:manual`/`r2` Seam-A enqueue earns a durable
        # TW row (M1.3), so the veneer's `task import` needs a writable TASKDATA + an existing TASKRC (an
        # empty rc suffices — the veneer passes every setting as an `rc.*` override). Without these,
        # taskwarrior refuses ("Cannot proceed without rc file") and the enqueue never lands its row.
        export TASKDATA="$RIG_ROOT/task/data"
        export TASKRC="$RIG_ROOT/task/.taskrc"
        mkdir -p "$TASKDATA"
        [ -f "$TASKRC" ] || : > "$TASKRC"
        mkdir -p \
          "$XDG_RUNTIME_DIR/tally" \
          "$XDG_STATE_HOME/tally/events" \
          "$XDG_DATA_HOME/tally/mock-artifacts" \
          "$XDG_CONFIG_HOME/tally"
      '';

      # The daemon process: boot `tally daemon run` against the scratch tree. Restart on failure so
      # a transient bind race self-heals; the socket appearing is its readiness signal.
      daemonCmd = pkgs.writeShellApplication {
        name = "tally-dev-daemon";
        # `mockPls` puts the granting `pls` on PATH so the Seam-A enqueue's lease→dispatch→evidence→
        # witness path runs for real (the OCR rehearsal), not the smoke's fake-worker fallback;
        # `taskwarrior3` provides the `task` binary the durable-row veneer shells out to.
        runtimeInputs = [ tally pkgs.coreutils mockPls pkgs.taskwarrior3 ];
        text = ''
          ${rigEnv}
          exec tally daemon run
        '';
      };

      # The scripted enqueuer + smoke: wait for the socket, assert session.snapshot, drop the OCR
      # sample and run one mock job to completion. `MOCK` is the in-store copy of dev/mock.
      enqueueCmd = pkgs.writeShellApplication {
        name = "tally-dev-enqueue";
        runtimeInputs = [ tally pkgs.bash pkgs.coreutils pkgs.gawk pkgs.python3 pkgs.socat ];
        text = ''
          ${rigEnv}
          MOCK="${mockSrc}/mock"
          export FAKE_WORKER="$MOCK/fake-worker.sh"
          exec bash "$MOCK/enqueue-samples.sh"
        '';
      };
    in
    {
      # process-compose-flake renders `packages.dev` (the `process-compose up` wrapper) and —
      # because of the `test` process — `checks.dev-test`. This pinned rev does NOT auto-populate
      # `apps.dev`, so we export it explicitly below pointing at `packages.dev`, matching the
      # documented `apps.dev` output (SPEC "Flake outputs"; BUILD-SEQUENCE step 1) so
      # `nix run .#dev` resolves regardless of the app/package precedence.
      apps.dev = {
        type = "app";
        program = lib.getExe self'.packages.dev;
      };

      process-compose."dev" = {
        # A process named `test` is excluded from `up` and becomes the flake check.
        settings.processes = {
          daemon = {
            command = lib.getExe daemonCmd;
            availability = {
              restart = "on_failure";
              backoff_seconds = 2;
            };
            readiness_probe = {
              # Resolve the socket path the SAME way rigEnv does (the probe runs in process-compose's
              # own env, NOT the daemon's shell, so it must repeat the XDG_RUNTIME_DIR fallback — in the
              # `nix flake check` sandbox $XDG_RUNTIME_DIR is unset/unwritable, so both the daemon and
              # this probe fall back to `$TMPDIR/tally-rig-$(id -u)`; a naive `${XDG_RUNTIME_DIR:-/run/
              # user/$(id -u)}` here would check the wrong path and the probe would never pass).
              exec.command = ''
                rd="''${XDG_RUNTIME_DIR:-}"
                if [ -z "$rd" ] || [ ! -w "$rd" ]; then rd="''${TMPDIR:-/tmp}/tally-rig-$(id -u)"; fi
                test -S "$rd/tally/tally.sock"
              '';
              initial_delay_seconds = 1;
              period_seconds = 1;
              timeout_seconds = 2;
              success_threshold = 1;
              failure_threshold = 30;
            };
          };

          enqueue = {
            # SMOKE_TAG namespaces this driver's events/artifact files so it never collides with the
            # `test` driver (they run concurrently in the check variant).
            command = "SMOKE_TAG=enqueue ${lib.getExe enqueueCmd}";
            depends_on.daemon.condition = "process_healthy";
            availability.restart = "no";
          };

          # The flake-check gate: same smoke, run after the daemon is healthy. process-compose-flake
          # enables this process ONLY in the test variant (excluded from `up`) and stamps it with
          # `exit_on_end`/`exit_on_skipped`, so when the smoke completes the whole rig tears down and
          # `nix flake check` proves the rig boots + one mock job completes. A distinct SMOKE_TAG
          # keeps its files disjoint from the concurrent `enqueue` driver.
          test = {
            command = "SMOKE_TAG=test ${lib.getExe enqueueCmd}";
            depends_on.daemon.condition = "process_healthy";
            availability.restart = "no";
          };
        };
      };
    };
}
