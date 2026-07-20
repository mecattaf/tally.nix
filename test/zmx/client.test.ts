// tally — zmx enumerate-only client tests (IMPLEMENTATION-PLAN M1.6: zmx list parse).
//
// Asserts the ZmxClient reads `zmx list --short` and NOTHING ELSE, parses the one-name-per-line
// contract into `persistence_session_id`s, and that the forbidden lifecycle verbs are refused (the
// dotfiles-owned boundary, CLI-SURFACE §3.2 MUST-NOT list). Driven against the layer-0 FakeZmx.

import { describe, expect, test } from "bun:test";
import { FakeExec } from "../helpers/exec-fakes.ts";
import { FakeZmx } from "../helpers/fake-zmx.ts";
import { ZmxClient, parseListShort, FORBIDDEN_ZMX_VERBS } from "../../src/zmx/client.ts";
import { TallyError } from "../../src/contracts/errors.ts";

function setup() {
  const exec = new FakeExec();
  const zmx = new FakeZmx();
  zmx.install(exec);
  const client = new ZmxClient(exec);
  return { exec, zmx, client };
}

describe("parseListShort", () => {
  test("one session name per line ⇒ persistence_session_ids", () => {
    const out = parseListShort("term-0707-1530\nterm-0708-0900\n");
    expect(out).toEqual([{ name: "term-0707-1530" }, { name: "term-0708-0900" }]);
  });

  test("tolerates no trailing newline and blank lines", () => {
    expect(parseListShort("a\n\n  b  \n")).toEqual([{ name: "a" }, { name: "b" }]);
    expect(parseListShort("solo")).toEqual([{ name: "solo" }]);
  });

  test("empty output ⇒ no sessions", () => {
    expect(parseListShort("")).toEqual([]);
    expect(parseListShort("\n\n")).toEqual([]);
  });
});

describe("ZmxClient.listShort", () => {
  test("enumerates the session universe via `zmx list --short`", async () => {
    const { exec, zmx, client } = setup();
    zmx.add("term-0707-1530", "term-0708-0900");
    const sessions = await client.listShort();
    expect(sessions.map((s) => s.name)).toEqual(["term-0707-1530", "term-0708-0900"]);
    expect(exec.lastCall("zmx")!.argv).toEqual(["zmx", "list", "--short"]);
  });

  test("names() projects to bare persistence_session_ids", async () => {
    const { zmx, client } = setup();
    zmx.add("s1", "s2", "s3");
    expect(await client.names()).toEqual(["s1", "s2", "s3"]);
  });

  test("reflects a session ending", async () => {
    const { zmx, client } = setup();
    zmx.add("s1", "s2");
    zmx.remove("s1");
    expect(await client.names()).toEqual(["s2"]);
  });

  test("empty universe ⇒ []", async () => {
    const { client } = setup();
    expect(await client.listShort()).toEqual([]);
  });
});

describe("the boundary: zmx lifecycle is forbidden", () => {
  test("assertReadOnlyVerb throws for every forbidden lifecycle verb", () => {
    for (const verb of FORBIDDEN_ZMX_VERBS) {
      expect(() => ZmxClient.assertReadOnlyVerb(verb)).toThrow(TallyError);
    }
  });

  test("assertReadOnlyVerb allows the read verb", () => {
    expect(() => ZmxClient.assertReadOnlyVerb("list")).not.toThrow();
  });

  test("the FakeZmx throws loudly if any code path reaches a lifecycle verb", async () => {
    const exec = new FakeExec();
    const zmx = new FakeZmx().install(exec);
    // Directly exercising the fake with attach must reject (mirrors the boundary law).
    await expect(exec.run(["zmx", "attach", "term-0707-1530", "fish"])).rejects.toThrow(/forbidden/);
    expect(zmx.forbiddenAttempts.length).toBeGreaterThan(0);
  });

  test("the ZmxClient surface has no lifecycle methods", () => {
    const { client } = setup();
    const surface = Object.getOwnPropertyNames(Object.getPrototypeOf(client));
    for (const verb of FORBIDDEN_ZMX_VERBS) {
      expect(surface).not.toContain(verb);
    }
  });
});
