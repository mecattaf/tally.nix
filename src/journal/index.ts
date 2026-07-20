// tally — the journal module barrel (M1.4). The journald TALLY_* emission (writer) + read path
// (reader): the observability leg of the four-log read-time join (SPEC "journald TALLY_* event
// schema", "The four-log read-time join"; IMPLEMENTATION-PLAN §3 Journald).
//
// journald is observability, NOT load-bearing memory — the witness ledger is a separate artifact
// emitted from the same fields. The writer (`emit.ts`) turns one lifecycle transition into a single
// structured stdout line captured by `StandardOutput=journal`; the reader (`reader.ts`) shells
// `journalctl -t tally -o json` back into re-hydrated `TallyFields` for `query log` and standup.

export {
  JournalEmitter,
  stdoutSink,
  toFields,
  validateFields,
  renderLine,
  type EmitEvent,
  type JournalSink,
} from "./emit.ts";

export {
  JournalReader,
  parseLine,
  buildArgv,
  buildFollowArgv,
  type JournalEntry,
  type ReadOptions,
} from "./reader.ts";
