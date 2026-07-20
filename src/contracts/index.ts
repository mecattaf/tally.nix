// tally — the shared-contracts barrel (IMPLEMENTATION-PLAN M0.2). Every parallel implementer
// imports from `src/contracts` (this module) and from its declared `dependsOn` modules only. This
// re-exports the whole agreement surface: the frozen wire contract, events, snapshot, witness,
// journald matrix, TW veneer types, job/agent enums, selectors, config, paths, constants, errors,
// and the injectable seams (Exec, Clock, Bus, DaemonMount, the snapshot/wait providers).

export * from "./constants";
export * from "./errors";
export * from "./agent";
export * from "./job";
export * from "./task";
export * from "./witness";
export * from "./journal";
export * from "./snapshot";
export * from "./events";
export * from "./selectors";
export * from "./config";
export * from "./paths";
export * from "./exec";
export * from "./bus";
export * from "./wire";
