// tally — the witness module barrel (IMPLEMENTATION-PLAN M1.2). The append-only witness JSONL,
// ledger-as-truth: physical append (`O_APPEND` + fsync per line), the restart-surviving per-line
// hash chain, the daemonless verifier, model-id normalization, and record validation/projection.
//
// Consumers: `jobs` (M2.2) appends heavy-unit lines and reads the LSN; `cli` (M3.1) `witness verify`
// runs the daemonless verifier; `query standup` reads the canonical-GPU-seconds aggregation.

export {
  WitnessLedger,
  openLedger,
  scanChainHead,
  type RecoverScan,
} from "./ledger";

export {
  sha256Hex,
  computeHash,
  buildRecord,
  advanceHead,
  serializeLine,
  GENESIS_HEAD,
  type WitnessBody,
  type ChainHead,
} from "./chain";

export {
  parseRecord,
  canonicalGpuSeconds,
  isKnownVerdict,
  isKnownLaborClass,
  toProjection,
  countsTowardCanonicalGpuSeconds,
  type ParseResult,
  type WitnessProjection,
} from "./record";

export {
  verifyRecords,
  verifyLedgerFile,
  type VerifyReport,
  type VerifyProblem,
} from "./verify";

export { normalizeModelId } from "./model-id";
