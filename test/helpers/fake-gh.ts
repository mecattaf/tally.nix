// test/helpers/fake-gh.ts
//
// A fake of the ambient authenticated `gh` CLI, covering exactly the read
// surface tally's gh intake polls (CLI-SURFACE §3 / octo.nvim surface scan):
//
//   gh api /notifications                 -> the notifications fixture
//   gh api graphql -f query=...           -> the search fixture (issue/PR search)
//   gh api /rate_limit                    -> a configurable rate-limit budget
//   gh api /repos/.../pulls/<n>           -> per-item hydration (echo from a map)
//
// Auth is ambient (DECISIONS Q8): the fake never manages credentials; it just
// serves canned JSON. Rate-limit headroom is programmable so the backoff test
// can drive the "remaining == 0" branch. By default it serves the checked-in
// fixtures (test/fixtures/gh/*.json); override with `setNotifications` /
// `setSearch` for bespoke cases.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { type FakeExec, type ExecResult, ok, fail, okJson, parseArgs } from "./exec-fakes.ts";

const HERE = dirname(fileURLToPath(import.meta.url));
const FIXTURES = join(HERE, "..", "fixtures", "gh");

function loadFixture(name: string): unknown {
  return JSON.parse(readFileSync(join(FIXTURES, name), "utf8"));
}

export interface RateLimit {
  remaining: number;
  limit: number;
  reset: number;
}

/**
 * A programmable gh fake. Defaults to the checked-in fixtures; mutate the canned
 * responses and rate-limit budget before installing.
 */
export class FakeGh {
  private notifications: unknown = loadFixture("notifications.json");
  private search: unknown = loadFixture("search.json");
  private hydrations = new Map<string, unknown>();
  private rate: RateLimit = { remaining: 4999, limit: 5000, reset: 0 };
  /** Every gh call, decomposed (for two-phase-probe assertions). */
  readonly calls: Array<{ kind: string; detail: string }> = [];

  setNotifications(value: unknown): this {
    this.notifications = value;
    return this;
  }

  setSearch(value: unknown): this {
    this.search = value;
    return this;
  }

  /** Register a per-item hydration response keyed by endpoint path. */
  setHydration(path: string, value: unknown): this {
    this.hydrations.set(path, value);
    return this;
  }

  /** Set rate-limit headroom (drive the backoff branch with remaining:0). */
  setRateLimit(rate: Partial<RateLimit>): this {
    this.rate = { ...this.rate, ...rate };
    return this;
  }

  /** Count of graphql search calls (proves two-phase: no hydration w/o delta). */
  graphqlCount(): number {
    return this.calls.filter((c) => c.kind === "graphql").length;
  }

  /** Count of hydration (per-item) calls. */
  hydrationCount(): number {
    return this.calls.filter((c) => c.kind === "hydrate").length;
  }

  install(exec: FakeExec): this {
    exec.register("gh", (args): ExecResult => {
      // tally always calls `gh api <endpoint> [flags]`.
      if (args[0] !== "api") return fail(2, `fake-gh: only 'gh api' supported, got '${args[0]}'`);
      const rest = args.slice(1);
      const parsed = parseArgs(rest);
      const endpoint = parsed.positionals[0] ?? "";

      // graphql (search / two-phase probe).
      if (endpoint === "graphql") {
        this.calls.push({ kind: "graphql", detail: parsed.value("f") ?? "" });
        // Emit rate-limit exhaustion as gh would (non-zero exit + message).
        if (this.rate.remaining <= 0) {
          return fail(1, "API rate limit exceeded");
        }
        return okJson(this.search);
      }

      if (endpoint === "/notifications" || endpoint === "notifications") {
        this.calls.push({ kind: "notifications", detail: endpoint });
        if (this.rate.remaining <= 0) return fail(1, "API rate limit exceeded");
        return okJson(this.notifications);
      }

      if (endpoint === "/rate_limit" || endpoint === "rate_limit") {
        this.calls.push({ kind: "rate_limit", detail: endpoint });
        return okJson({
          resources: {
            core: { ...this.rate },
            graphql: { ...this.rate },
            search: { ...this.rate },
          },
          rate: { ...this.rate },
        });
      }

      // Per-item hydration (a specific repo/pulls/issues endpoint).
      if (this.hydrations.has(endpoint)) {
        this.calls.push({ kind: "hydrate", detail: endpoint });
        if (this.rate.remaining <= 0) return fail(1, "API rate limit exceeded");
        return okJson(this.hydrations.get(endpoint));
      }

      // A `gh --version` probe some code runs at startup.
      if (endpoint === "" && (parsed.has("version") || rest.includes("--version"))) {
        return ok("gh version 2.55.0");
      }

      // Unknown endpoints: 404-shaped (gh exits 1 with a JSON error).
      this.calls.push({ kind: "unknown", detail: endpoint });
      return fail(1, JSON.stringify({ message: "Not Found", status: "404" }));
    });
    return this;
  }
}
