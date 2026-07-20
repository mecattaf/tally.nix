// tally — the jobs module barrel (IMPLEMENTATION-PLAN M2.2). The spawn-tracked-agent-job — the one
// execution primitive (SPEC "Three planes"). Composes Seam-A admission (enqueue + dedup-by-existence),
// the priority-queue → pls-lease → transient-unit dispatch, the evidence gate, the terminal commit
// (witness line + journald + TW completion + delta), recover() (re-present, never replay),
// preemption-as-policy (cooperative yield), and the `--wait`/barrier primitive into one engine.
//
// Consumers: `main.ts` constructs the engine and calls `mount(daemon)` (the composition root wires
// the `queue.*` RPC carriers + the `jobs[]` snapshot section provider); the CLI (M3.1) reaches Seam A
// and the queue verbs through those carriers; daemon-core/wait.ts reaches a job-subject `session.wait`
// through the engine's barrier tracker.

export {
  JobsEngine,
  type JobsEngineDeps,
  type QueueOpResult,
} from "./engine";

export {
  admit,
  adapterFor,
  resolvePool,
  isGpuPool,
  rowSeedFor,
  DEFAULT_HEAVY_POOL,
  type AdmittedJob,
} from "./enqueue";

export {
  probeDedup,
  hashFile,
  artifactPaths,
  declaredHash,
  ArrayWitnessSource,
  type DedupResult,
  type DedupProbe,
  type WitnessSource,
} from "./dedup";

export {
  runEvidenceGate,
  checkedPaths,
  type RunOutcome,
  type GateResult,
  type CheckOutcome,
} from "./evidence";

export {
  PriorityQueue,
  TransientRunner,
  acquireLease,
  priorityRank,
  unitName,
  unitNameFor,
  jobEnv,
  ENV_TASK_UUID,
  ENV_SESSION_REF,
  ENV_YIELD_FD,
  ENV_LEASE_EPOCH,
  ENV_JOB_ID,
  type DispatchExecResult,
  type DispatchExecOptions,
} from "./dispatch";

export {
  canTransition,
  isTerminal,
  transition,
  newJobEntry,
  toJobRecord,
  TERMINAL_STATES,
  type JobEntry,
  type LifecycleSinks,
  type JournalExtra,
} from "./lifecycle";

export {
  BarrierTracker,
  BarrierError,
  verdictExitCode,
  type TerminalDelta,
  type JobWaitResult,
  type JobWaitTimeout,
} from "./barrier";

export {
  shouldPreempt,
  markPreempted,
  prepareResume,
  YieldSignaller,
  YIELD_SIGNAL,
  type PreemptionDecision,
} from "./preempt";

export {
  planRecovery,
  executeReclaims,
  DEFAULT_ATTEMPT_CAP,
  type RecoveryPlan,
  type RecoverInput,
  type RepresentPlan,
  type ReclaimPlan,
  type AbandonPlan,
  type LiveLease,
} from "./recover";

export {
  splitInvocation,
  resolveCommand,
  type AgentAdapter,
  type AdapterContext,
  type LeafInvocation,
  type RunExtract,
} from "../agents/kinds";

export { piAdapter, PiAdapter } from "../agents/pi";
export { claudeCodeAdapter, ClaudeCodeAdapter } from "../agents/claude-code";
export { shellAdapter, ShellAdapter } from "../agents/shell";
