// tally CLI — the `pane` noun: the minimal kitty-native binding, keyed on kitty_window_id
// (IMPLEMENTATION-PLAN M3.1; CLI-SURFACE §1.3). Every verb is a thin socket request; the selector is
// resolved daemon-side (the daemon holds the live model). tally observes panes — it never launches.
//
//   tally pane send     <sel> <text> [--enter]
//   tally pane send-key <sel> <key>
//   tally pane focus    <sel>
//   tally pane capture  <sel> [--source visible|recent|detection] [--lines <n>] [--format text|ansi]
//
// `pane capture --source detection` refuses `is_viewer` panes (mirrors the detector's exclusion —
// anti-loop invariant #4); the refusal surfaces as a `viewer_rejected` wire error the CLI maps to a
// non-zero exit.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { parseSelector } from "../contracts/selectors";
import { connectClient } from "./client";
import { printError, printJson, printLine, type Writer } from "./output";
import { flag, hasFlag, wantsJson, type CliContext } from "./index";
import { clientOpts } from "./queue";

/** Route the `pane` noun. */
export async function runPane(ctx: CliContext): Promise<number> {
  switch (ctx.verb) {
    case "send":
      return doSend(ctx);
    case "send-key":
      return doSendKey(ctx);
    case "focus":
      return doFocus(ctx);
    case "capture":
      return doCapture(ctx);
    default:
      printError(ctx.writer, `unknown pane verb '${ctx.verb ?? "(none)"}' (expected send|send-key|focus|capture)`);
      return 2;
  }
}

/** The selector is the first positional; validated syntactically here, resolved daemon-side. */
function requireSelector(ctx: CliContext): string | null {
  const raw = ctx.args.positionals[0];
  if (raw === undefined || raw.length === 0) {
    printError(ctx.writer, `${ctx.noun} ${ctx.verb} requires a <selector>`);
    return null;
  }
  // Syntactic validation (throws nothing; the daemon does the model resolution).
  parseSelector(raw);
  return raw;
}

async function doSend(ctx: CliContext): Promise<number> {
  const sel = requireSelector(ctx);
  if (sel === null) return 2;
  let text = ctx.args.positionals[1];
  if (text === undefined) {
    printError(ctx.writer, "pane send requires <text>");
    return 2;
  }
  // `--enter` appends a carriage return so the sent text is submitted (kitty send-text convention).
  if (hasFlag(ctx.args, "--enter")) text = text + "\r";

  const client = await connectClient(clientOpts(ctx));
  try {
    const result = await client.call<{ pane: string; kitty_window_id: number; sent: boolean }>("pane.send", {
      pane: sel,
      text,
    });
    emitSent(ctx.writer, result, wantsJson(ctx.args), `sent to ${result.pane}`);
    return 0;
  } finally {
    client.close();
  }
}

async function doSendKey(ctx: CliContext): Promise<number> {
  const sel = requireSelector(ctx);
  if (sel === null) return 2;
  const key = ctx.args.positionals[1];
  if (key === undefined) {
    printError(ctx.writer, "pane send-key requires a <key> (e.g. enter, esc, ctrl+c)");
    return 2;
  }
  const client = await connectClient(clientOpts(ctx));
  try {
    const result = await client.call<{ pane: string; kitty_window_id: number; key: string; sent: boolean }>("pane.send_key", {
      pane: sel,
      keys: key,
    });
    if (wantsJson(ctx.args)) printJson(ctx.writer, result);
    else printLine(ctx.writer, `sent key '${key}' to ${result.pane}`);
    return 0;
  } finally {
    client.close();
  }
}

async function doFocus(ctx: CliContext): Promise<number> {
  const sel = requireSelector(ctx);
  if (sel === null) return 2;
  const client = await connectClient(clientOpts(ctx));
  try {
    const result = await client.call<{ pane: string; kitty_window_id: number; focused: boolean }>("pane.focus", {
      pane: sel,
    });
    if (wantsJson(ctx.args)) printJson(ctx.writer, result);
    else printLine(ctx.writer, `focused ${result.pane} (win=${result.kitty_window_id})`);
    return 0;
  } finally {
    client.close();
  }
}

async function doCapture(ctx: CliContext): Promise<number> {
  const sel = requireSelector(ctx);
  if (sel === null) return 2;
  const source = flag(ctx.args, "--source") ?? "visible";
  if (source !== "visible" && source !== "recent" && source !== "detection") {
    printError(ctx.writer, `unknown --source '${source}' (expected visible|recent|detection)`);
    return 2;
  }
  const format = flag(ctx.args, "--format") ?? "text";
  if (format !== "text" && format !== "ansi") {
    printError(ctx.writer, `unknown --format '${format}' (expected text|ansi)`);
    return 2;
  }
  const params: { pane: string; source: string; format: string; lines?: number } = { pane: sel, source, format };
  const linesRaw = flag(ctx.args, "--lines");
  if (linesRaw !== undefined) {
    const n = Number(linesRaw);
    if (!Number.isInteger(n) || n <= 0) {
      printError(ctx.writer, `--lines must be a positive integer: ${linesRaw}`);
      return 2;
    }
    params.lines = n;
  }

  const client = await connectClient(clientOpts(ctx));
  try {
    const result = await client.call<{ pane: string; kitty_window_id: number; source: string; lines: number; text: string }>(
      "pane.capture",
      params,
    );
    if (wantsJson(ctx.args)) {
      printJson(ctx.writer, result);
    } else {
      // The captured text is the payload — print it raw (no `tally:` prefix) so it is pipeable.
      ctx.writer.out(result.text.endsWith("\n") ? result.text : result.text + "\n");
    }
    return 0;
  } finally {
    client.close();
  }
}

function emitSent(w: Writer, result: { pane: string }, json: boolean, human: string): void {
  if (json) printJson(w, result);
  else printLine(w, human);
}
