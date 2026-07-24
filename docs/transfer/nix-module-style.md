# Nix module style transfer: microvm.nix + srvos

Source repos (cloned `--depth 1`, pinned to the commit noted):

- `~/Downloads/microvm.nix` — github:astro/microvm.nix @ `0784796` (2026-07-18, merge #558)
- `~/Downloads/srvos` — github:nix-community/srvos @ `327a43d` (2026-07-23)

Scope: informs two tally.nix additions — `services.tally.flows.<name>` (declarative
workflow registrations rendering to calendar-producer argv) and per-adapter systemd
hardening presets. All paths below are relative to the two repo roots unless a full
path is given.

---

## 1. microvm.nix option-tree anatomy

microvm.nix splits its interface into two independently-imported module trees, which
is the load-bearing idea to lift for `flows.<name>`:

- **`nixos-modules/host/`** — lives on the *orchestrating* machine. Defines
  `microvm.vms.<name>` (host/options.nix:30-167), an `attrsOf submodule` where each
  instance describes how to obtain a *guest* configuration (inline `config`, a
  `flake`, or a pre-`evaluatedConfig`), not the VM's runtime behavior itself.
- **`nixos-modules/microvm/`** — imported *inside* the guest's own NixOS evaluation
  (or synthesized on the host via `import eval-config.nix` when using the `config`
  option). Defines the actual `microvm.*` interface: `hypervisor`, `vcpu`, `mem`,
  `interfaces`, `volumes`, `shares`, etc. (nixos-modules/microvm/options.nix, ~1050
  lines). `nixos-modules/microvm/default.nix:11-26` imports all the guest-facing
  submodules (`boot-disk.nix`, `store-disk.nix`, `mounts.nix`, `interfaces.nix`,
  `pci-devices.nix`, `virtiofsd/`, `graphics.nix`, `rosetta.nix`, `optimization.nix`,
  `ssh-deploy.nix`, `vsock-ssh.nix`) plus `asserts.nix` and `system.nix`.

**The submodule-with-custom-merge pattern** (host/options.nix:42-72) is the sharpest
technique here: `vms.<name>.config` is typed as
`nullOr (lib.mkOptionType { name = "Toplevel NixOS config"; merge = ...; })`. Its
`merge` function doesn't just combine values — it calls `import eval-config.nix`
right there, injecting `../microvm` (the guest module tree) as a forced module and
setting `prefix = [ "microvm" "vms" name "config" ]` so error messages inside the
nested guest evaluation point back to the right host option path. This is how one
option accepts "a NixOS module" and evaluates it as a nested, fully-typed
configuration without a separate `evalModules` call site scattered through the
codebase.

**Interchangeable backends over one declarative interface**: the guest option
`hypervisor` is `types.enum self-lib.hypervisors` (nixos-modules/microvm/options.nix:51-59),
where `hypervisors` is a flat list defined once in `lib/default.nix:3-12`
(`qemu`, `cloud-hypervisor`, `firecracker`, `crosvm`, `kvmtool`, `stratovirt`,
`alioth`, `vfkit`). Every backend is built *eagerly, for all of them*, not just the
selected one:

```nix
# nixos-modules/microvm/default.nix:29-38
config.microvm.runner = lib.genAttrs microvm-lib.hypervisors (hypervisor:
  microvm-lib.buildRunner {
    inherit pkgs;
    microvmConfig = config.microvm // { inherit (config.networking) hostName; inherit hypervisor; };
    inherit (config.system.build) toplevel;
  }
);
```

and only then is the active one selected:

```nix
# nixos-modules/microvm/options.nix:979-984
declaredRunner = mkOption {
  type = types.package;
  default = config.microvm.runner.${config.microvm.hypervisor};
};
```

`buildRunner` (`lib/runner.nix:19-21`) dynamically imports
`./runners + "/${microvmConfig.hypervisor}.nix"` — one file per backend
(`lib/runners/{qemu,cloud-hypervisor,firecracker,crosvm,kvmtool,stratovirt,alioth,vfkit}.nix`).
Each backend file is handed the *same* generic `microvmConfig` attrset (interfaces,
volumes, shares, devices, vcpu, mem, kernel, initrdPath, etc.) and returns an ad hoc
but consistent interface: `{ command, canShutdown, shutdownCommand, preStart?,
supportsNotifySocket?, tapMultiQueue?, requiresMacvtapAsFds?, setBalloonScript? }`
(confirmed in `lib/runners/firecracker.nix:83-onward`). `runner.nix:23-27` reads
these as optional attrs with `or false`/`or null` defaults, so a backend only needs
to implement the parts of the interface it actually supports — this is the
"submodule interface, N backends" contract in its purest form: no backend module
declares options, they just consume config and produce a fixed-shape result record.

This is the direct analogue for tally's adapter map: one declarative
`flows.<name>`/adapter config, N backend renderers, each renderer file returning
the same small result shape (e.g. `{ command, unitExtras, ... }`), selected by an
enum tied to a single canonical list (mirror `lib/default.nix`'s `hypervisors` list
— don't let the enum type and the backend-file directory drift independently).

**Assertion style, with file:line examples** (both host- and guest-side):

- Mutual exclusivity, actionable message:
  `nixos-modules/host/default.nix:19-27` —
  `"vm ${vmName}: Fully-declarative VMs cannot also set a flake!"` /
  `"...cannot set a updateFlake!"`, generated via `concatMap` over all configured VM
  names so every instance is checked, not just a global toggle.
- Uniqueness-by-derived-key, using `builtins.groupBy` + assert-group-length-1, repeated
  three times for three different keys in `nixos-modules/microvm/asserts.nix:8-30,60-70`:
  volume `image` path, interface `id`, share `tag` — the idiom is
  `map (xs: { assertion = length xs == 1; message = "... used ${length xs} > 1 times"; }) (attrValues (groupBy (x: x.key) config.list))`.
  Directly reusable for validating that pool names / flow names don't collide.
- Cross-field conditional assertion:
  `nixos-modules/microvm/asserts.nix:33-49` — interface `type == "bridge"` implies
  `bridge != null`, and the converse, both with distinct messages naming the
  offending interface `id`.
- Backend-specific assertion gated by the selected enum value:
  `nixos-modules/microvm/asserts.nix:110-116` —
  `lib.optionals (config.microvm.hypervisor == "cloud-hypervisor") [ { assertion = ...; message = ...; } ]`.
  This is the pattern for "only validate pool-existence semantics that are relevant
  to the selected adapter backend."
- Deprecation-as-eval-error: `nixos-modules/microvm/options.nix:1053-1055` uses
  `lib.mkRemovedOptionModule ["microvm" "balloonMem"] "..."` rather than a bare
  assertion, giving users a standard NixOS-flavored removal message.

---

## 2. srvos hardening bundles

**Correction against the assumed premise**: srvos does not contain a *library* of
multiple named, swappable hardening presets. There is exactly one concrete,
fully-worked hardening bundle in the repo, applied to one service type — the
GitHub Actions runner — plus one generic per-instance override escape hatch. Grepped
`ProtectHome|ProtectSystem|PrivateTmp|NoNewPrivileges|hardening|serviceConfig` across
the whole tree; it only matched `darwin/common/nix.nix`, `nixos/common/nix.nix`
(unrelated: OOM score / nix-daemon tuning, no sandboxing directives) and the two
github-runners files below.

**File**: `nixos/modules/github-runners/service.nix:301-365` (module function is
parameterized as `{ config, lib, pkgs, cfg, svcName, ... }` and instantiated once per
configured runner instance — see composition below). The full directive set:

```
AmbientCapabilities        = "";
CapabilityBoundingSet      = "";
DeviceAllow                = "";           # ProtectClock adds DeviceAllow=char-rtc r
NoNewPrivileges            = true;
PrivateDevices             = true;
PrivateMounts              = true;
PrivateTmp                 = true;
PrivateUsers               = true;
ProtectClock               = true;
ProtectControlGroups       = true;
ProtectHome                = true;
ProtectHostname             = true;
ProtectKernelLogs          = true;
ProtectKernelModules       = true;
ProtectKernelTunables      = true;
ProtectSystem              = "strict";
RemoveIPC                  = true;
RestrictNamespaces         = true;
RestrictRealtime           = true;
RestrictSUIDSGID           = true;
UMask                      = "0066";
ProtectProc                = "invisible";
SystemCallFilter           = [ "~@clock" "~@cpu-emulation" "~@module" "~@mount"
                                "~@obsolete" "~@raw-io" "~@reboot" "~capset"
                                "~setdomainname" "~sethostname" ];
RestrictAddressFamilies    = [ "AF_INET" "AF_INET6" "AF_UNIX" "AF_NETLINK" ];
PrivateNetwork             = false;   # needs network access — commented as an explicit exception
MemoryDenyWriteExecute     = false;   # "Cannot be true due to Node" — commented exception
ProcSubset                 = "all";   # commented: "pid" breaks `nix` commands' /proc/stat read
LockPersonality            = false;   # commented: coverage tooling (cargo-tarpaulin) needs personality syscall
DynamicUser                = true;
```
(exact line range: service.nix:304-364). Note the recurring convention: every
directive that deviates from the "obviously strictest" setting carries an inline
comment explaining *why* (Node needs W^X violated, nix needs /proc, coverage tooling
needs personality) — worth carrying into tally's preset vocabulary verbatim as a
documentation habit, since these are exactly the kinds of exceptions an
adapter-hardening preset will need to make per-adapter.

Directory/capability-adjacent hardening also present in the same block:
`InaccessiblePaths` (service.nix:291-297) conditionally hides the token file and
GitHub App private key file from the unit's view (`optionalString (!isNull
cfg.tokenFile) "-${cfg.tokenFile}"` — the leading `-` makes the path optional so
eval doesn't fail if it's absent); `StateDirectory`/`RuntimeDirectory`/`LogsDirectory`
(service.nix:283-289) use systemd's own directory-management directives rather than
manual `mkdir`/`chown` in `preStart` (contrast with microvm.nix's `install-microvm-*`
service in host/default.nix:127-137, which does its own `mkdir`+`chown`+`ln`
scripting — two different, both legitimate, styles worth knowing before picking
one for tally's adapter units).

**Composition/override mechanism**: `serviceOverrides` is a plain
`types.attrs` option (`nixos-modules/github-runners/options.nix:162-171`,
example given is `{ ProtectHome = false; }`) merged in at the very end of the
`serviceConfig` attrset with `//`:

```nix
# service.nix:365-367
}
// (lib.optionalAttrs (cfg.user != null) { User = cfg.user; })
// cfg.serviceOverrides;
```

So "compose" here means: one hard-coded baseline bundle, and "override" means a
last-write-wins shallow attrset merge supplied per-instance by the caller — there is
no preset-selection enum, no bundle registry, no bundle-to-bundle inheritance. If
tally wants a *vocabulary* of named presets (e.g. `strict`/`network-adapter`/
`filesystem-adapter`), that's a tally-original design — srvos gives the single-bundle
shape and the trailing-override-merge idiom, not a multi-preset library. Don't
describe srvos as having that; it doesn't.

**Multi-instance wiring** (the part that *is* directly reusable for
per-adapter units): `nixos-modules/github-runners/default.nix` defines
`services.srvos-github-runners` as `attrsOf submodule` (options imported
parametrically via `import ./options.nix (args // {...})`, line 19), then in
`config.systemd.services` (default.nix:58-75) does
`flip mapAttrs' cfg (n: v: nameValuePair "github-runner-${n}" (import ./service.nix (args // { inherit svcName; cfg = v // {...}; systemdDir = "github-runner/${n}"; })))`
— i.e. the service *module function* is imported once per configured instance with
`cfg` and naming context injected as extra function arguments, rather than templated
string interpolation into a `systemd.services."x@".{...}` template unit (contrast
with microvm.nix's `%i`-templated instantiated units in
`nixos-modules/host/default.nix:186-318`). Two legitimate idioms for "one config
entry → one systemd unit, N times": microvm.nix uses systemd template units
(`foo@.service` + `foo@bar`), srvos uses Nix-level `mapAttrs'` to generate N
distinctly-named concrete units. For `flows.<name>` → one calendar-triggered unit
per flow, the srvos `mapAttrs'`-generates-named-units approach is the better match
(flows are heterogeneous per-name, not homogeneous instances of one template).

---

## 3. Eval-time validation idioms (both repos)

- **`config.assertions`** is the workhorse in both repos; every assertion is data
  (`{ assertion = bool; message = str; }`), collected via `map`/`concatMap`/
  `mapAttrsToList` over the relevant attrset/list, so eval fails immediately with all
  actionable messages rather than failing at build or runtime. Concrete instances:
  microvm.nix `nixos-modules/host/default.nix:19-28`,
  `nixos-modules/microvm/asserts.nix:7-116`, `nixos-modules/microvm/system.nix:5-9`;
  srvos `nixos/modules/github-runners/default.nix:48-56` (the
  `tokenFile` XOR `githubApp` check — exactly the shape needed for "pool named by a
  script's meta block must exist in config": `mapAttrsToList (name: c: { assertion =
  ...; message = "${name}: ..."; }) cfg`, i.e. iterate the attrsOf-submodule, name
  the specific offending instance in the message).
- **Type-level constraints doing validation work**: `types.enum self-lib.hypervisors`
  (microvm/options.nix:52) rejects unknown hypervisor names at option-set time, before
  any assertion runs, with a auto-generated "does not match" error. `types.ints.positive`,
  `types.port`, `nonEmptyStr`, `path` throughout microvm/options.nix similarly push
  simple validity checks into the type rather than a hand-written assertion — cheaper
  and gives a more standard NixOS error. Use enum types for pool-name-shaped or
  backend-shaped fields wherever the valid set is closed and known at option-declaration
  time; reserve assertions for validation that depends on *other* option values
  (cross-referencing script meta against configured pools is inherently this kind,
  since it's config-against-config, not value-against-fixed-set).
- **Custom `mkOptionType` with eval-driving `merge`**: `nixos-modules/host/options.nix:47-72`
  — the sharpest idiom in the corpus. A `merge` function is allowed to do arbitrary
  work (here, a full nested `import eval-config.nix` evaluation) rather than just
  concatenating/last-writing values. This is how microvm.nix gets "accept a NixOS
  module as an option value, evaluate it inline, surface its own assertions/errors
  with a rewritten `prefix`" without a bespoke top-level `evalModules` call. Directly
  applicable if `flows.<name>` ever needs to accept an inline script/module value
  and eval-validate it against the pool set right there rather than deferring to a
  generic assertion list.
- **`checks` derivations** are eval-time only in the sense that *building* them
  re-runs Nix evaluation of full NixOS configurations and (for srvos) boots a
  NixOS VM test; they are not pure `nix eval` assertions. srvos:
  `dev/checks.nix` builds `system.build.toplevel` for every configuration in
  `dev/test-configurations.nix` (so any option misuse anywhere in the
  common/server/desktop trees fails at `nix flake check` time) plus one
  `nixosTest.makeTest` boot-and-`wait_for_unit "sshd.service"` smoke test
  (`dev/checks.nix:12-27`). microvm.nix: `checks/` has one file per behavior
  (`vm.nix`, `startup-shutdown.nix`, `shutdown-command.nix`, `machined.nix`,
  `iperf.nix`, `imperative-template.nix`, `microvm-command.nix`, `shellcheck.nix`) —
  each a NixOS VM test asserting one hypervisor-backend behavior actually works, not
  just evaluates. Lift the "one toplevel-eval check per representative configuration"
  half of this (cheap, catches option misuse broadly); the VM-boot half is only worth
  copying if tally ends up wanting integration tests for adapter units, which is a
  separate, heavier decision.

---

## 4. Lift list and do-not-copy

**Lift** (read these directly during implementation):

- `~/Downloads/microvm.nix/nixos-modules/host/options.nix` (all, esp. 30-167) —
  `attrsOf submodule` pattern + custom `mkOptionType` merge trick for
  `flows.<name>`'s "accept a store-path-producing script + validate it" shape.
- `~/Downloads/microvm.nix/nixos-modules/microvm/asserts.nix` — assertion idioms
  (uniqueness-by-key, cross-field, backend-gated); template for the
  pools-referenced-by-meta-block existence check.
- `~/Downloads/microvm.nix/lib/runner.nix` and one representative backend
  (`~/Downloads/microvm.nix/lib/runners/firecracker.nix`, the shortest full one) —
  the "same config in, ad hoc fixed-shape result record out, selected by enum" backend
  contract, for tally's per-adapter renderer map.
  `~/Downloads/microvm.nix/lib/default.nix:1-12` — the single-source-of-truth enum
  list backing that dispatch.
- `~/Downloads/microvm.nix/nixos-modules/microvm/default.nix:28-39` — the
  `genAttrs hypervisors (hypervisor: buildRunner {...})` eager-build-all-then-select
  pattern; decide deliberately whether tally wants this (build every adapter
  renderer for every flow) or lazy dispatch (only build the selected adapter) —
  microvm.nix's choice is defensible because these are cheap script derivations, but
  cost may differ for tally's adapter argv renderers.
- `~/Downloads/srvos/nixos/modules/github-runners/service.nix:280-368` — the
  hardening-bundle-plus-serviceOverrides-merge idiom, and the commented-exception
  documentation habit.
- `~/Downloads/srvos/nixos/modules/github-runners/options.nix:162-171` and
  `default.nix` (whole file) — `attrsOf submodule` + per-instance
  `import ./service.nix (args // { cfg = v; ... })` unit generation; the model for
  "one `flows.<name>` entry → one named, non-templated systemd unit."
- `~/Downloads/srvos/dev/checks.nix` — cheap "eval every representative
  configuration's toplevel" check pattern for catching option misuse in CI.

**Do NOT copy**:

- microvm.nix's *hypervisor-selection-as-single-flat-enum-with-N-parallel-files*
  literally 1:1 — it works because all 8 backends share an (almost) identical input
  shape (interfaces/volumes/shares/devices/vcpu/mem). Tally's adapters likely have
  more heterogeneous per-adapter option surfaces (per the existing adapter map), so
  forcing every adapter renderer through one undifferentiated `microvmConfig`-style
  bag risks becoming a junk-drawer attrset. Take the *dispatch* idiom, not the
  *one-shared-config-bag* idiom.
- microvm.nix's manual `mkdir`/`ln -sTf`/`chown` scripting in
  `install-microvm-${name}` (host/default.nix:111-147) as the template for how tally
  should manage adapter unit state directories — srvos's `StateDirectory=`/
  `RuntimeDirectory=`/`LogsDirectory=` systemd-native directives
  (service.nix:283-289) are the safer, less-code default; only reach for manual
  scripting if systemd's directory directives genuinely can't express what's needed
  (microvm.nix needs it because it's managing a symlink-swap for atomic "current"
  pointer semantics, which is a real reason — don't copy the mechanism without also
  copying that reason).
- Do not describe or implement srvos "hardening presets" as if a multi-preset
  library already exists there — it doesn't (see §2). Copying the *shape* of a
  single hardcoded-bundle-plus-override is fine; inventing a false lineage of
  "srvos's strict/relaxed/whatever presets" in tally's docs would be citing
  something that isn't in the source.
- microvm.nix's `nix.optimise.automatic`-conflict-with-`writableStoreOverlay`
  guard-as-plain-assertion (`nixos-modules/microvm/system.nix:5-9`) is fine as an
  idiom but the specific check is domain-specific to store overlays — don't
  transplant the literal condition, just the "assert two options aren't
  simultaneously incompatible" shape (already covered generically above).
- srvos's `with lib;` at the top of `nixos-modules/github-runners/{default,service}.nix`
  and `options.nix` — convenient for reading srvos itself, but importing `lib`
  unqualified is exactly the kind of implicit-namespace pattern that tends to fight
  linters/rust-analogous-style discipline; prefer `lib.foo` explicit qualification
  in tally's own modules regardless of what the source repos do stylistically.
