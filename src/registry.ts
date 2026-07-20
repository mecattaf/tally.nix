// tally — the entrypoint REGISTRY (the composition seam, extracted to a leaf module).
//
// `main.ts` dispatches the two roles (`daemon` / `cli`) through this registry so the compiled binary
// can compose the real daemon + CLI without `main.ts` statically importing daemon-core/cli internals.
// The registry lives in ITS OWN leaf module (imported by nothing else in `src/`) precisely so the
// self-registering CLI (`cli/index.ts` calls `registerEntrypoint("cli", …)` at module load) can read a
// fully-initialized `registry` even when the import graph is cyclic: were the registry a `const` in
// `main.ts`, the cycle `main → entrypoints → cli → main` would hit `registry` in its temporal dead zone
// during the compiled binary's module evaluation. As a leaf, this module is evaluated to completion
// before any importer's body runs, so `registry.set(...)` is always safe.
//
// Owned by module M0.1 `scaffold`. Do not add real RPC surface here — that is daemon-core's.

/** A tally entrypoint: consumes the post-`daemon`/CLI argv and resolves to a process exit code. */
export type Entrypoint = (argv: string[]) => number | Promise<number>;

/** The two dispatch roles. `argv[1] === "daemon"` selects `"daemon"`, everything else `"cli"`. */
export type EntrypointRole = "daemon" | "cli";

const registry = new Map<EntrypointRole, Entrypoint>();

/**
 * Register a real entrypoint for a role. Called by the layer-1 (daemon) / layer-3 (cli) composition
 * roots so `main.ts` can dispatch to them without a static import. The last registration for a role
 * wins (a module fully replaces the layer-0 fallback).
 */
export function registerEntrypoint(role: EntrypointRole, fn: Entrypoint): void {
  registry.set(role, fn);
}

/** The registered entrypoint for a role, if any (`main.ts` falls back to its layer-0 stub otherwise). */
export function resolveEntrypoint(role: EntrypointRole): Entrypoint | undefined {
  return registry.get(role);
}
