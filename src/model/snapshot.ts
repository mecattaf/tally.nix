// tally — the session-model's `SnapshotProvider` (IMPLEMENTATION-PLAN M2.1 `snapshot.ts`, risk 9;
// CLI-SURFACE §2.2). session-model registers ONE `SnapshotProvider` with daemon-core; daemon-core
// owns the transport (it stamps the authoritative `lease_epoch`/`seq`/`ts`/`daemon_version` header
// over the returned frame — see daemon-core `assembleSnapshot`), and this provider assembles the
// §2.2 frame BODY from the single `store.ts`:
//
//   protocol / protocol_version / daemon_version / lease_epoch / seq / ts   (header — daemon-core)
//   focus / workspaces / sessions / panes / agents / jobs                   (body — the store)
//
// The `agents[]` and `jobs[]` legs are the ones the DETECTOR and JOBS wrote into the store via the
// Bus (the single-store ruling); this provider reads them off the store, never importing the
// detector or jobs modules. `snapshot.ts` is a thin adapter — the composition lives in `store.ts`
// so there is exactly one assembler.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import type { Snapshot, SnapshotProvider } from "../contracts/snapshot";
import type { SessionStore } from "./store";

/**
 * The session-model's snapshot provider. Registered with daemon-core (via `registerSnapshotProvider`)
 * so a `session.snapshot` request is answered from the single store. `snapshot()` returns the frame
 * BODY assembled from the store; daemon-core overwrites the transport header afterwards, so this
 * provider deliberately seeds only placeholder header values (the store's `composeSnapshot` does).
 */
export class SnapshotAssembler implements SnapshotProvider {
  constructor(private readonly store: SessionStore) {}

  /** Assemble the current §2.2 frame body from the single store (header stamped by daemon-core). */
  snapshot(): Snapshot {
    return this.store.composeSnapshot();
  }
}

/** Convenience: build the assembler for a store. */
export function makeSnapshotProvider(store: SessionStore): SnapshotProvider {
  return new SnapshotAssembler(store);
}
