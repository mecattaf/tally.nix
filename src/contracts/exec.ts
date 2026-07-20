// tally — the `Exec` subprocess seam and the `Clock` seam (IMPLEMENTATION-PLAN §2 rules, §3 Seams).
//
// ALL subprocess access (kitty, zmx, task, pls, gh, journalctl, git, systemd-run) goes through
// this injectable seam, so every module is testable against the layer-0 fakes without the real
// substrate. No module ever calls `Bun.spawn` directly — it takes an `Exec` in its constructor.

/** Options for one subprocess invocation. */
export interface ExecOptions {
  /** stdin to feed the process (string or bytes). */
  stdin?: string | Uint8Array;
  /** Working directory. */
  cwd?: string;
  /** Environment overrides (merged over the ambient env by the implementation). */
  env?: Record<string, string>;
  /** Millisecond timeout; the implementation kills the process and rejects on overrun. */
  timeoutMs?: number;
}

/** The result of a completed subprocess. */
export interface ExecResult {
  code: number;
  stdout: string;
  stderr: string;
  /** True when the process was killed by the `timeoutMs` guard. */
  timedOut?: boolean;
}

/**
 * A long-running / streaming subprocess handle (e.g. `journalctl -f`, a spawned agent unit). Lines
 * arrive via the async iterator over stdout; `kill` terminates it; `exited` resolves with the code.
 */
export interface ExecStream {
  /** Async iterator over stdout lines (NDJSON consumers iterate this directly). */
  lines(): AsyncIterableIterator<string>;
  /** Write to the process stdin. */
  write(data: string | Uint8Array): Promise<void>;
  /** Terminate the process (default SIGTERM; pass a signal to override). */
  kill(signal?: NodeJS.Signals | number): void;
  /** Resolves with the exit code when the process exits. */
  exited: Promise<number>;
  /** The process id, when known. */
  pid?: number;
}

/**
 * The injectable subprocess runner (IMPLEMENTATION-PLAN §3 Seams). The ONLY way any module shells
 * out. Fakes (`testkit`) implement this interface with scripted responses.
 */
export interface Exec {
  /** Run a command to completion, capturing stdout/stderr and the exit code. */
  run(argv: string[], opts?: ExecOptions): Promise<ExecResult>;
  /** Spawn a streaming/long-running command (follow tails, agent units). */
  spawn(argv: string[], opts?: ExecOptions): ExecStream;
}

/**
 * The injectable clock (IMPLEMENTATION-PLAN §3 Seams). Modules take this rather than calling
 * `Date.now()` / `setTimeout` directly, so tests drive time deterministically (heartbeat cadence,
 * detector throttle, wait timeouts).
 */
export interface Clock {
  /** Milliseconds since the epoch. */
  now(): number;
  /** An ISO-8601 timestamp for the current instant. */
  nowIso(): string;
  /** Resolve after `ms` milliseconds. */
  sleep(ms: number): Promise<void>;
  /** Schedule `fn` after `ms`; returns a canceller. */
  setTimer(ms: number, fn: () => void): () => void;
  /** Schedule `fn` every `ms`; returns a canceller. */
  setInterval(ms: number, fn: () => void): () => void;
}

/** The real system clock — the production `Clock`. */
export const systemClock: Clock = {
  now: () => Date.now(),
  nowIso: () => new Date().toISOString(),
  sleep: (ms: number) => new Promise((resolve) => setTimeout(resolve, ms)),
  setTimer: (ms: number, fn: () => void) => {
    const h = setTimeout(fn, ms);
    return () => clearTimeout(h);
  },
  setInterval: (ms: number, fn: () => void) => {
    const h = setInterval(fn, ms);
    return () => clearInterval(h);
  },
};
