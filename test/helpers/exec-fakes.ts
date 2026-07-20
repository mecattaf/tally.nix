// test/helpers/exec-fakes.ts
//
// The core of the tally testkit: a fake implementation of the injectable `Exec`
// subprocess seam (src/contracts/exec.ts) plus a router that dispatches the
// sanctioned subprocess families (kitty, zmx, task, pls, gh, journalctl,
// systemd-run) to per-binary fake handlers.
//
// Every layer >= 1 module shells out ONLY through `Exec`, so wiring a module
// against `FakeExec` makes it fully testable with no real substrate. The fakes
// in the sibling files (fake-kitty.ts, fake-zmx.ts, ...) register command
// handlers on a `FakeExec` and record every invocation for assertions.
//
// `FakeExec` implements the FULL contract `Exec` interface (both `run` and the
// streaming `spawn`), so it is a genuine drop-in wherever a module takes an
// `Exec` in its constructor.
//
// This file is authored fresh for tally; nothing is copied from vendor/
// (herdr is AGPL, cmux is GPL — clean-room law, CLI-SURFACE §4).

import type { Exec, ExecOptions, ExecResult, ExecStream } from "../../src/contracts/exec.ts";

export type { Exec, ExecOptions, ExecResult, ExecStream };

/** One recorded invocation, for assertions. */
export interface ExecInvocation {
  readonly argv: readonly string[];
  readonly opts: ExecOptions;
  readonly result: ExecResult;
  readonly at: number;
}

/**
 * A handler for one binary. Receives the argument vector *after* argv[0] (the
 * binary name), plus the call options, and returns the process result. Throwing
 * a non-`ExecResult` is treated as an internal test error and re-thrown.
 */
export type CommandHandler = (
  args: readonly string[],
  opts: ExecOptions,
) => ExecResult | Promise<ExecResult>;

/**
 * A handler for a streaming binary (`journalctl -f`, an agent unit). Returns the
 * lines it will stream, plus an optional exit code. If a binary has no stream
 * handler, `spawn` falls back to running its `run` handler once and streaming
 * the captured stdout as lines.
 */
export type StreamHandler = (
  args: readonly string[],
  opts: ExecOptions,
) => { lines: string[]; code?: number } | Promise<{ lines: string[]; code?: number }>;

/** Convenience constructors for the common result shapes. */
export function ok(stdout = "", stderr = ""): ExecResult {
  return { code: 0, stdout, stderr };
}

export function fail(code: number, stderr = "", stdout = ""): ExecResult {
  return { code, stdout, stderr };
}

export function okJson(value: unknown): ExecResult {
  return { code: 0, stdout: JSON.stringify(value), stderr: "" };
}

/**
 * A programmable fake `Exec`. Register a handler per binary basename; each
 * `run()` routes on `argv[0]`'s basename and records the call. Unregistered
 * binaries fail loudly so a test never silently exercises the real substrate.
 */
export class FakeExec implements Exec {
  private readonly handlers = new Map<string, CommandHandler>();
  private readonly streamHandlers = new Map<string, StreamHandler>();
  /** Every call, in order — the recorder every fake and test reads. */
  readonly calls: ExecInvocation[] = [];

  /** Register (or replace) the handler for one binary basename. */
  register(binary: string, handler: CommandHandler): this {
    this.handlers.set(binary, handler);
    return this;
  }

  /** Register a streaming handler for one binary basename (for `spawn`). */
  registerStream(binary: string, handler: StreamHandler): this {
    this.streamHandlers.set(binary, handler);
    return this;
  }

  /** True when a handler is registered for `binary`. */
  has(binary: string): boolean {
    return this.handlers.has(binary);
  }

  async run(argv: string[], opts: ExecOptions = {}): Promise<ExecResult> {
    if (argv.length === 0) {
      throw new Error("FakeExec.run: empty argv");
    }
    const binary = basename(argv[0]!);
    const handler = this.handlers.get(binary);
    if (!handler) {
      throw new Error(
        `FakeExec: no handler registered for binary '${binary}' ` +
          `(argv: ${JSON.stringify(argv)}). Register a fake before running.`,
      );
    }
    const result = await handler(argv.slice(1), opts);
    this.calls.push({ argv: [...argv], opts, result, at: this.calls.length });
    return result;
  }

  spawn(argv: string[], opts: ExecOptions = {}): ExecStream {
    if (argv.length === 0) {
      throw new Error("FakeExec.spawn: empty argv");
    }
    const binary = basename(argv[0]!);
    const stream = this.streamHandlers.get(binary);
    const runHandler = this.handlers.get(binary);
    if (!stream && !runHandler) {
      throw new Error(
        `FakeExec: no stream/run handler for binary '${binary}' ` +
          `(argv: ${JSON.stringify(argv)}).`,
      );
    }
    return new FakeExecStream(async () => {
      if (stream) {
        const { lines, code } = await stream(argv.slice(1), opts);
        return { lines, code: code ?? 0 };
      }
      // Fall back to the one-shot run handler: split its stdout into lines.
      const result = await runHandler!(argv.slice(1), opts);
      this.calls.push({ argv: [...argv], opts, result, at: this.calls.length });
      const lines = result.stdout.length
        ? result.stdout.split("\n").filter((l) => l.length > 0)
        : [];
      return { lines, code: result.code };
    });
  }

  /** All invocations whose binary basename matches. */
  callsFor(binary: string): ExecInvocation[] {
    return this.calls.filter((c) => basename(c.argv[0]!) === binary);
  }

  /** The most recent invocation of `binary`, or undefined. */
  lastCall(binary: string): ExecInvocation | undefined {
    const matches = this.callsFor(binary);
    return matches[matches.length - 1];
  }

  /** Flattened argv of every call (for grep-style assertions). */
  commandLines(): string[] {
    return this.calls.map((c) => c.argv.join(" "));
  }

  /** Reset the recorded call log without dropping handlers. */
  clearCalls(): void {
    this.calls.length = 0;
  }
}

/** A streaming handle over a pre-computed / lazily-computed line list. */
class FakeExecStream implements ExecStream {
  pid = 424242;
  exited: Promise<number>;
  private killed = false;
  private resolveExit!: (code: number) => void;
  private readonly ready: Promise<{ lines: string[]; code: number }>;

  constructor(produce: () => Promise<{ lines: string[]; code: number }>) {
    this.exited = new Promise<number>((resolve) => {
      this.resolveExit = resolve;
    });
    this.ready = produce();
  }

  async *lines(): AsyncIterableIterator<string> {
    const { lines, code } = await this.ready;
    for (const line of lines) {
      if (this.killed) break;
      yield line;
    }
    if (!this.killed) this.resolveExit(code);
  }

  async write(_data: string | Uint8Array): Promise<void> {
    // No-op: fakes do not model interactive stdin on a stream.
  }

  kill(_signal?: NodeJS.Signals | number): void {
    this.killed = true;
    this.resolveExit(143); // 128 + SIGTERM
  }
}

/** POSIX basename without importing node:path (keeps the compile lean). */
export function basename(p: string): string {
  const trimmed = p.endsWith("/") ? p.slice(0, -1) : p;
  const idx = trimmed.lastIndexOf("/");
  return idx === -1 ? trimmed : trimmed.slice(idx + 1);
}

/**
 * Parse a `--flag value` / `--flag=value` / positional argument vector into a
 * small structured form the per-binary fakes reuse. Repeatable flags accumulate.
 * Bare `--flag` (no following value, or followed by another flag) is a boolean.
 */
export interface ParsedArgs {
  readonly positionals: string[];
  readonly flags: Record<string, string[]>;
  readonly bools: Set<string>;
  has(flag: string): boolean;
  value(flag: string): string | undefined;
  values(flag: string): string[];
}

export function parseArgs(args: readonly string[]): ParsedArgs {
  const positionals: string[] = [];
  const flags: Record<string, string[]> = {};
  const bools = new Set<string>();
  const push = (k: string, v: string) => {
    (flags[k] ??= []).push(v);
  };
  for (let i = 0; i < args.length; i++) {
    const a = args[i]!;
    if (a === "--") {
      // Everything after `--` is a raw argv passthrough (positionals).
      for (let j = i + 1; j < args.length; j++) positionals.push(args[j]!);
      break;
    }
    if (a.startsWith("--")) {
      const eq = a.indexOf("=");
      if (eq !== -1) {
        push(a.slice(2, eq), a.slice(eq + 1));
        continue;
      }
      const key = a.slice(2);
      const next = args[i + 1];
      if (next === undefined || next.startsWith("-")) {
        bools.add(key);
      } else {
        push(key, next);
        i++;
      }
    } else if (a.startsWith("-") && a.length > 1) {
      // Short flag; treat as boolean unless a value follows.
      const key = a.slice(1);
      const next = args[i + 1];
      if (next === undefined || next.startsWith("-")) {
        bools.add(key);
      } else {
        push(key, next);
        i++;
      }
    } else {
      positionals.push(a);
    }
  }
  return {
    positionals,
    flags,
    bools,
    has: (f) => bools.has(f) || f in flags,
    value: (f) => flags[f]?.[0],
    values: (f) => flags[f] ?? [],
  };
}
