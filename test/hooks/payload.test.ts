// test/hooks/payload.test.ts
//
// The hook payload scripts (M3.2): the `agent.hook_event` NDJSON frame they post is valid against the
// daemon's own ingress validator (`src/detector/hooks.ts validateHookEventParams`), the CC/pi
// lifecycle mapping is correct, and posting to an ABSENT socket resolves fast without throwing (the
// harness must never block on tally).

import { test, expect, afterEach } from "bun:test";
import { createServer, type Server } from "node:net";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { validateHookEventParams } from "../../src/detector/hooks";

import {
  buildPayload as buildCcPayload,
  postHookEvent as postCc,
  parseStdin,
  resolveEvent,
  asCcHookEvent,
} from "../../hooks/claude-code/tally-hook";
import {
  buildPayload as buildPiPayload,
  activate as piActivate,
  emit as piEmit,
  resolveSessionRef,
  type HookPost as PiHookPost,
  type PiContext,
} from "../../hooks/pi/tally-session";

const cleanups: Array<() => void> = [];
afterEach(() => {
  while (cleanups.length) cleanups.pop()!();
});

/** A one-shot Unix-socket server that captures the first NDJSON frame posted to it. */
function captureServer(): { path: string; received: Promise<any>; server: Server } {
  const dir = mkdtempSync(join(tmpdir(), "tally-hookpost-"));
  const path = join(dir, "tally.sock");
  cleanups.push(() => rmSync(dir, { recursive: true, force: true }));

  let resolveFrame!: (v: any) => void;
  const received = new Promise<any>((r) => (resolveFrame = r));

  const server = createServer((sock) => {
    let buf = "";
    sock.setEncoding("utf8");
    sock.on("data", (chunk: string) => {
      buf += chunk;
      const nl = buf.indexOf("\n");
      if (nl !== -1) {
        const line = buf.slice(0, nl);
        resolveFrame(JSON.parse(line));
        sock.end();
      }
    });
  });
  server.listen(path);
  cleanups.push(() => server.close());
  return { path, received, server };
}

// --- Claude Code payload shape --------------------------------------------------------------------

test("cc UserPromptSubmit posts a frame valid against the daemon validator", async () => {
  const { path, received } = captureServer();
  const payload = buildCcPayload("UserPromptSubmit", { session_id: "cc-sess-1", cwd: "/work/repo" });
  await postCc(payload, 500, path);

  const frame = await received;
  expect(frame.method).toBe("agent.hook_event");
  expect(typeof frame.id).toBe("string");
  // The daemon accepts it and normalizes to HookEventParams.
  const parsed = validateHookEventParams(frame.params);
  expect(parsed.kind).toBe("claude-code");
  expect(parsed.turn).toBe("UserPromptSubmit");
  expect(parsed.lifecycle).toBe("running");
  expect(parsed.session_ref).toBe("cc-sess-1");
  expect(parsed.cwd).toBe("/work/repo");
});

test("cc lifecycle mapping: Stop→idle, Notification→needsInput, SessionStart→no lifecycle", () => {
  expect(buildCcPayload("Stop", {}).lifecycle).toBe("idle");
  expect(buildCcPayload("Notification", {}).lifecycle).toBe("needsInput");
  expect(buildCcPayload("SessionStart", {}).lifecycle).toBeUndefined();
  // Each still carries its turn tag and validates.
  for (const ev of ["Stop", "Notification", "SessionStart"] as const) {
    const p = buildCcPayload(ev, { session_id: "s" });
    expect(() => validateHookEventParams(p)).not.toThrow();
    expect(validateHookEventParams(p).turn).toBe(ev);
  }
});

test("cc stdin parsing is defensive (bad json ⇒ {}) and event resolution honors env + stdin", () => {
  expect(parseStdin("")).toEqual({});
  expect(parseStdin("not json")).toEqual({});
  expect(parseStdin("[1,2]")).toEqual({});
  expect(parseStdin('{"a":1}')).toEqual({ a: 1 });

  expect(asCcHookEvent("Stop")).toBe("Stop");
  expect(asCcHookEvent("Bogus")).toBeNull();
  // stdin hook_event_name is the fallback when env is unset.
  const prev = process.env.CLAUDE_HOOK_EVENT;
  delete process.env.CLAUDE_HOOK_EVENT;
  try {
    expect(resolveEvent({ hook_event_name: "Notification" })).toBe("Notification");
    expect(resolveEvent({})).toBeNull();
  } finally {
    if (prev !== undefined) process.env.CLAUDE_HOOK_EVENT = prev;
  }
});

// --- pi payload shape -----------------------------------------------------------------------------

test("pi turnStart posts a frame valid against the daemon validator, carrying the resume ref", async () => {
  const { path, received } = captureServer();
  const ctx: PiContext = { sessionId: "pi-sess-9", cwd: "/pi/work" };
  await piEmit("turnStart", ctx, (p) => postPi(p, path));

  const frame = await received;
  const parsed = validateHookEventParams(frame.params);
  expect(parsed.kind).toBe("pi");
  expect(parsed.turn).toBe("UserPromptSubmit");
  expect(parsed.lifecycle).toBe("running");
  expect(parsed.session_ref).toBe("pi-sess-9");
});

test("pi lifecycle mapping and session-ref resolution", () => {
  expect(buildPiPayload("turnEnd", "s", null).lifecycle).toBe("idle");
  expect(buildPiPayload("needsInput", "s", null).lifecycle).toBe("needsInput");
  expect(buildPiPayload("sessionStart", "s", null).lifecycle).toBeUndefined();
  // session ref via either the flat id or the nested session.id.
  expect(resolveSessionRef({ sessionId: "flat" })).toBe("flat");
  expect(resolveSessionRef({ session: { id: "nested" } })).toBe("nested");
  expect(resolveSessionRef({})).toBeNull();
  // All pi payloads validate against the daemon.
  for (const sig of ["turnStart", "turnEnd", "needsInput", "sessionStart"] as const) {
    expect(() => validateHookEventParams(buildPiPayload(sig, "s", "/c"))).not.toThrow();
  }
});

test("pi activate registers lifecycle handlers and emits an immediate sessionStart", async () => {
  const posts: PiHookPost[] = [];
  const capture = async (p: PiHookPost) => {
    posts.push(p);
  };
  const handlers = new Map<string, () => void>();
  const ctx: PiContext = {
    sessionId: "pi-boot",
    on: (event, handler) => {
      handlers.set(event, handler as () => void);
    },
  };
  const registered = piActivate(ctx, capture);
  expect(registered).toBeGreaterThan(0);
  // The immediate sessionStart announcement fired.
  await Promise.resolve();
  expect(posts.some((p) => p.turn === "SessionStart" && p.session_ref === "pi-boot")).toBe(true);

  // A registered turn handler drives a working post.
  handlers.get("turnStart")?.();
  await Promise.resolve();
  await Promise.resolve();
  expect(posts.some((p) => p.lifecycle === "running")).toBe(true);
});

// --- socket-absent fast-exit ----------------------------------------------------------------------

test("posting to an absent socket resolves without throwing (cc)", async () => {
  const dir = mkdtempSync(join(tmpdir(), "tally-nosock-"));
  cleanups.push(() => rmSync(dir, { recursive: true, force: true }));
  const dead = join(dir, "does-not-exist.sock");
  const start = Date.now();
  await expect(postCc(buildCcPayload("Stop", {}), 250, dead)).resolves.toBeUndefined();
  // It gives up quickly (well under the 250ms timeout for a refused/absent socket).
  expect(Date.now() - start).toBeLessThan(2000);
});

test("posting to an absent socket resolves without throwing (pi), and emit swallows failures", async () => {
  const dir = mkdtempSync(join(tmpdir(), "tally-nosock-pi-"));
  cleanups.push(() => rmSync(dir, { recursive: true, force: true }));
  const dead = join(dir, "does-not-exist.sock");
  await expect(postPi(buildPiPayload("turnEnd", "s", null), dead)).resolves.toBeUndefined();
  // emit() must never reject even if the injected post throws.
  await expect(
    piEmit("turnStart", { sessionId: "s" }, async () => {
      throw new Error("boom");
    }),
  ).resolves.toBeUndefined();
});

// helper: the pi post seam with an explicit path (mirrors postCc's arg order).
import { postHookEvent as piPost } from "../../hooks/pi/tally-session";
function postPi(p: PiHookPost, path: string): Promise<void> {
  return piPost(p, 500, path);
}
