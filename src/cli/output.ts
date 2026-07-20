// tally CLI — output rendering (IMPLEMENTATION-PLAN M3.1; CLI-SURFACE §0 "JSON output is the
// contract, text output is a convenience", `--json` everywhere).
//
// One place owns how a verb's result reaches stdout: the `--json` one-record-per-line projection
// (the contract), a human-readable text convenience, and the `jsonl`/`tree`/`md` projections a few
// verbs expose (`session watch --format`, `query render --format`, `query standup --format`). The
// CLI writes through the injectable `Writer` seam so tests capture output without spawning a process.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

/** The output sink the CLI writes through (injectable for tests). */
export interface Writer {
  out(text: string): void;
  err(text: string): void;
}

/** The process stdout/stderr writer — the production sink. */
export const processWriter: Writer = {
  out: (text: string) => {
    process.stdout.write(text);
  },
  err: (text: string) => {
    process.stderr.write(text);
  },
};

/** A capturing writer for tests: accumulates stdout/stderr into strings. */
export function captureWriter(): Writer & { stdout: string; stderr: string } {
  const w = {
    stdout: "",
    stderr: "",
    out(text: string) {
      w.stdout += text;
    },
    err(text: string) {
      w.stderr += text;
    },
  };
  return w;
}

/** Print a value as a single JSON line (the `--json` contract). */
export function printJson(w: Writer, value: unknown): void {
  w.out(JSON.stringify(value) + "\n");
}

/** Print each element of an array as its own JSON line (the one-record-per-line projection). */
export function printJsonLines(w: Writer, records: readonly unknown[]): void {
  for (const r of records) w.out(JSON.stringify(r) + "\n");
}

/** Print a plain text line. */
export function printLine(w: Writer, text: string): void {
  w.out(text + "\n");
}

/** Print an error line to stderr, prefixed `tally:`. */
export function printError(w: Writer, message: string): void {
  const m = message.startsWith("tally:") ? message : `tally: ${message}`;
  w.err(m + "\n");
}

/**
 * Render a Workspace→Session→Pane tree as indented text (the `--format tree` convenience for
 * `session watch`/`session list`/`query render`). The shape is intentionally simple: workspaces,
 * their sessions, and each session's panes with the agent kind/status dot. Consumers wanting the
 * canonical structure use `--json`.
 */
export interface TreeWorkspace {
  workspace: string;
  sessions: TreeSession[];
}
export interface TreeSession {
  session: string;
  status_rollup?: Record<string, number>;
  panes: TreePane[];
}
export interface TreePane {
  pane: string;
  kind?: string | null;
  status?: string | null;
}

const STATUS_DOT: Record<string, string> = {
  blocked: "!",
  working: "*",
  done: "+",
  idle: ".",
};

/** A single-char status dot for the text tree (unknown/absent ⇒ space). */
export function statusDot(status: string | null | undefined): string {
  if (!status) return " ";
  return STATUS_DOT[status] ?? "?";
}

/** Render the tree projection to indented text. */
export function renderTree(workspaces: readonly TreeWorkspace[]): string {
  const lines: string[] = [];
  for (const ws of workspaces) {
    lines.push(ws.workspace);
    for (const s of ws.sessions) {
      const roll = s.status_rollup
        ? ` [${(["blocked", "working", "done", "idle"] as const)
            .map((k) => `${STATUS_DOT[k]}${s.status_rollup![k] ?? 0}`)
            .join(" ")}]`
        : "";
      lines.push(`  ${s.session}${roll}`);
      for (const p of s.panes) {
        const kind = p.kind ? ` ${p.kind}` : "";
        lines.push(`    ${statusDot(p.status)} ${p.pane}${kind}`);
      }
    }
  }
  return lines.join("\n");
}
