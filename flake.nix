{
  description = "tally — agent-session orchestration: one Bun-compiled binary (daemon + CLI)";

  # Inputs pin only what tally leans on. Each substrate on its OWN named trigger, never bundled
  # (SPEC "Inputs & dev rig"; DECISIONS Q3). `pls`, `pi`, and `llama-swap` ship no root flake, so
  # each carries `flake = false` — without it `nix flake lock` chokes on the missing flake.nix
  # (layer-0 acceptance: `nix flake lock` resolves all named inputs). taskwarrior + gh resolve
  # from nixpkgs (upstream taskwarrior ships no root flake; `pkgs.taskwarrior3`).
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    flake-parts.url = "github:hercules-ci/flake-parts";
    flake-parts.inputs.nixpkgs-lib.follows = "nixpkgs";

    bun2nix.url = "github:baileyluTCD/bun2nix";
    bun2nix.inputs.nixpkgs.follows = "nixpkgs";

    process-compose-flake.url = "github:Platonic-Systems/process-compose-flake";

    # tally-pinned substrate — flake-less sources (verified: no root flake.nix in any).
    pls = {
      url = "github:sniarchos/pls";
      flake = false;
    };
    pi = {
      # ⚠ pin is stale per CLI-SURFACE §3.4/§5 flag 3 — adapters bind to the DOCUMENTED
      # interface, never this clone. Pinned so the box has a resolvable reference.
      url = "github:badlogic/pi";
      flake = false;
    };
    llama-swap = {
      # Pinned-only in v0; the runtime resolves from `pkgs.llama-swap` when present.
      url = "github:mostlygeek/llama-swap";
      flake = false;
    };
  };

  outputs =
    inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];

      # M3.4 dev-rig: process-compose-flake's flakeModule supplies the `process-compose."dev"`
      # option surface `nix/dev.nix` writes into, and `./nix/dev.nix` renders `packages.dev` /
      # `checks.dev-test` / `apps.dev`. The inline `devApp` placeholder is retired below.
      imports = [
        inputs.process-compose-flake.flakeModule
        ./nix/dev.nix
      ];

      perSystem =
        { system, pkgs, ... }:
        let
          # bun2nix's overlay supplies `pkgs.bun2nix` (with `.mkDerivation` / `.fetchBunDeps`).
          pkgsBun = import inputs.nixpkgs {
            inherit system;
            overlays = [ inputs.bun2nix.overlays.default ];
          };

          # ONE Bun-compiled binary = daemon + CLI (SPEC "Flake outputs").
          tally = pkgsBun.callPackage ./nix/package.nix { };

          # `packages.pls` — the tally-OWNED `pls` CLIENT the daemon shells out to. It is NOT a
          # verbatim wrap of the upstream `bin/pls`: upstream ships only `run|status|watch` over an
          # HTTP broker and has no `acquire|release|coalloc|--pool` surface, so wrapping it verbatim
          # left tally's entire lease→dispatch path dead in production (src/pls/broker.ts execs
          # `pls acquire ...`). This ships `nix/pls-shim.py`, which maps tally's client CLI onto the
          # upstream broker's HTTP API (POST /acquire, POST /release/<id>, GET /status), synthesizes
          # the `generation` (lease_epoch) counter, and resolves the per-pool broker URL from the env
          # the unit sets. `packages.plsBrokerSource` still exposes the upstream tree for the SERVER
          # units (server/app.py) that units.nix launches.
          pls = pkgs.writeShellApplication {
            name = "pls";
            runtimeInputs = [ pkgs.python3 ];
            text = ''
              exec ${pkgs.python3}/bin/python3 ${./nix/pls-shim.py} "$@"
            '';
          };

          # The upstream pls SERVER source tree (server/app.py) the broker units launch, packaged as a
          # python-runnable so the module's ExecStart is a concrete store path.
          plsBrokerSource = inputs.pls;

          # `apps.bun2nix` — wraps the bun2nix codegen binary so `nix run .#bun2nix` regenerates
          # `bun.nix` from `bun.lock` (BUILD-SEQUENCE step 1). bun2nix is a Nix-distributed
          # codegen binary, NOT an npm dep and NOT on PATH inside `nix shell nixpkgs#bun`; the
          # package.json `bun2nix` script execs this app. It writes bun.nix in the cwd.
          bun2nixApp = pkgs.writeShellApplication {
            name = "tally-bun2nix";
            runtimeInputs = [ inputs.bun2nix.packages.${system}.default ];
            # Default to regenerating ./bun.nix in place (from ./bun.lock) so both
            # `nix run .#bun2nix` and the package.json `bun2nix` script rewrite the committed
            # file. Any explicit args (e.g. `-o -` for stdout) pass straight through.
            text = ''
              if [ "$#" -eq 0 ]; then
                exec bun2nix --output-file ./bun.nix
              fi
              exec bun2nix "$@"
            '';
          };

        in
        {
          packages = {
            default = tally;
            inherit tally pls;
          };

          apps = {
            default = {
              type = "app";
              program = "${tally}/bin/tally";
            };
            bun2nix = {
              type = "app";
              program = "${bun2nixApp}/bin/tally-bun2nix";
            };
            # `apps.dev` is exported by ./nix/dev.nix (M3.4), pointing at the process-compose-flake
            # `packages.dev` wrapper — the "scaffold creates, dev-rig overwrites" handoff.
          };

          # `checks` — the packaged binary builds, and the pls client shim recognizes EVERY argv shape
          # src/pls/broker.ts emits (so the client CLI surface can never silently drift from the
          # broker again). The shim exits 1 (broker unreachable) — NOT 2 (unknown subcommand) — for a
          # recognized verb with no broker up; the check asserts exactly that for acquire/release/
          # status/coalloc.
          checks = {
            inherit tally;
            pls-cli-surface = pkgs.runCommand "pls-cli-surface" { } ''
              set -u
              fail=0
              check() {
                # $1 = verb, rest = args. A recognized verb without a broker exits 1 (unreachable);
                # an UNKNOWN verb exits 2. We require != 2. The `|| code=$?` keeps stdenv's `set -e`
                # from aborting on the expected non-zero (unreachable) exit.
                code=0
                PLS_URL="http://127.0.0.1:1" ${pls}/bin/pls "$@" >/dev/null 2>&1 || code=$?
                if [ "$code" = "2" ]; then
                  echo "FAIL: 'pls $*' is not a recognized subcommand (exit 2)"
                  fail=1
                fi
              }
              check acquire --pool worker-gpu --cost 1 --priority 100 --tenant tally
              check release --lease abc123
              check status --pool worker-gpu
              check coalloc --pools a,b --costs 1,1 --priority 50 --tenant tally
              if [ "$fail" = "1" ]; then exit 1; fi
              touch $out
            '';
          };

          formatter = pkgs.nixpkgs-fmt;
        };

      flake = {
        # -------------------------------------------------------------------------------------
        # homeManagerModules.tally — the PRIMARY, load-bearing module (SPEC "Flake outputs").
        # M3.3 `nix-module` (nix/hm-module.nix + nix/units.nix): typed options → generated
        # systemd user units + config.json + pls-lease-wrap. This flake wires the options the
        # module leaves for the flake to populate — `package`, `watcherScript`, the pls broker
        # `source`, and the daemon/drain `runtimeInputs` PATH — via a small defaults module
        # composed alongside it, so a bare `imports = [ tally.homeManagerModules.tally ]` in a
        # user's home.nix Just Works (they only set enable/role/conductorHost).
        # -------------------------------------------------------------------------------------
        homeManagerModules.tally = { lib, pkgs, ... }: {
          imports = [ ./nix/hm-module.nix ];

          # Flake-supplied option defaults (mkDefault so a user can still override any of them).
          # `pkgs.stdenv.hostPlatform.system` resolves the per-system package/inputs.
          config.services.tally =
            let
              system = pkgs.stdenv.hostPlatform.system;
              self = inputs.self;
            in
            {
              package = lib.mkDefault self.packages.${system}.tally;

              # The pls CLIENT shim (packages.pls) — added to the ambient wrap's own runtimeInputs and
              # to home.packages on both roles so `tally pls-wrap` always resolves `pls` (finding: the
              # wrap was installed without its runtime dependency, ENOENT-ing every ambient lease call).
              plsClient = lib.mkDefault self.packages.${system}.pls;

              # NOTE: `watcherScript` is a readOnly option whose value is the module's OWN default
              # (declared in nix/hm-module.nix as ../hooks/kitty/tally-watcher.py). The flake must NOT
              # DEFINE it here — a readOnly option rejects any definition (even mkDefault), which threw
              # "read-only, but it's set multiple times" the moment a consumer read it (DECISIONS Q4).

              # The pls broker's `flake = false` source tree (server/app.py). The module derives
              # the local-broker ExecStart from this when `plsBroker.command` is unset.
              plsBroker.source = lib.mkDefault inputs.pls;

              # PATH for the daemon/drain units' shell-outs (SPEC "Emission path" + veneers):
              # the tally binary, taskwarrior 3.x, the pls client wrap, gh, git, python3,
              # journalctl (systemd), coreutils. zmx + kitty stay ambient (dotfiles-owned).
              runtimeInputs = lib.mkDefault [
                self.packages.${system}.tally
                self.packages.${system}.pls
                pkgs.taskwarrior3
                pkgs.gh
                pkgs.git
                pkgs.python3
                pkgs.systemd
                pkgs.coreutils
              ];
            };
        };

        # -------------------------------------------------------------------------------------
        # nixosModules.tally — the ruled UNBUILT THIN WRAPPER STUB (PS#17/SPEC "Flake outputs").
        # A stub: everything tally owns is user-lifecycle; use homeManagerModules.tally.
        # -------------------------------------------------------------------------------------
        nixosModules.tally = ./nix/nixos-module.nix;
      };
    };
}
