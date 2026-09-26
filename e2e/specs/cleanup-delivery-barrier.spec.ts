import {
  test, expect, CLEANED_PROJECTS, cleanUpCreatedStories, healAtWorkerStart, healFixtureProjects, projectSlug,
  removeStrays, requiredEnv, storiesInProject,
} from "./support";
import type { APIRequestContext, TestInfo } from "@playwright/test";
import { BlockDeliveryBarrier, readBlockDeliverySnapshot } from "../block-delivery-barrier.cjs";
import { fixtureApiUrl } from "../fixture-api";
import { FIXTURE_BASELINE_ENV, fixtureBaseline } from "../fixture-baseline";
import { BASE_EXPECT_TIMEOUT_MS, gracedPatience } from "../load-grace";
import { execFileSync } from "node:child_process";
import { accessSync, constants, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { delimiter, dirname, join } from "node:path";

// Contracts for the cleanup fixture. The barrier cases use data snapshots,
// never a mock daemon, delivery worker, or terminal effect. The blocked-drop
// and status-destination browser cases exercise the shared barrier against
// all of those production paths in the same matrix. The SH-765 cases run the
// real cleanup against the real isolated store, with only python3's start
// made slow.

/** PATH as it was before a python3 shim went first on it; null when no shim
 * is active. */
let pathBeforeShim: string | undefined | null = null;

// Registered before the cleanup: hooks run in registration order, and a test
// that times out is abandoned mid-body, so its own `finally` has not restored
// PATH yet when the cleanup reads through python3.
test.afterEach(() => {
  if (pathBeforeShim === null) return;
  process.env.PATH = pathBeforeShim;
  pathBeforeShim = null;
});
cleanUpCreatedStories("Alpha Project");

const identity = [1, "fixture-uuid", "alpha-project", "AA", "/fixture", 171, "created"];
const snapshot = (rows: Array<[number, string, string]>) => ({ identity, deliveries: rows });
const row = (id: number, status: string): [number, string, string] => [id, "interrupt", status];

for (const terminal of ["delivered", "unreached", "uncertain", "superseded"]) {
  test(`cleanup waits for explicit ${terminal} acknowledgement`, () => {
    const barrier = new BlockDeliveryBarrier("alpha-project", "AA-171");
    expect(barrier.observe(snapshot([row(1, "pending")]))).toEqual(["1:pending"]);
    expect(barrier.observe(snapshot([row(1, "attempting")]))).toEqual(["1:attempting"]);
    expect(barrier.observe(snapshot([row(1, terminal)]))).toEqual([]);
  });
}

test("cleanup tracks every delivery, including one enqueued while waiting", () => {
  const barrier = new BlockDeliveryBarrier("alpha-project", "AA-171");
  expect(barrier.observe(snapshot([row(1, "attempting")]))).toEqual(["1:attempting"]);
  expect(barrier.observe(snapshot([row(1, "unreached"), row(2, "pending")]))).toEqual(["2:pending"]);
  expect(barrier.observe(snapshot([row(1, "unreached"), row(2, "delivered")]))).toEqual([]);
});

test("a missing observed delivery is never completion, even if it reappears", () => {
  const barrier = new BlockDeliveryBarrier("alpha-project", "AA-171");
  barrier.observe(snapshot([row(1, "attempting")]));
  expect(() => barrier.observe(snapshot([]))).toThrow(/delivery 1 disappeared/);
  expect(() => barrier.observe(snapshot([row(1, "delivered")]))).toThrow(/delivery 1 disappeared/);
});

test("unknown statuses cannot open the barrier", () => {
  const barrier = new BlockDeliveryBarrier("alpha-project", "AA-171");
  expect(() => barrier.observe(snapshot([row(1, "finished")]))).toThrow(/unknown delivery status/);
});

for (const [index, field] of ["project ID", "UUID", "slug", "prefix", "checkout", "story number", "creation identity"].entries()) {
  test(`cleanup refuses a changed ${field}`, () => {
    const barrier = new BlockDeliveryBarrier("alpha-project", "AA-171");
    barrier.observe(snapshot([row(1, "attempting")]));
    const replacement = [...identity];
    replacement[index] = "replacement";
    expect(() => barrier.observe({ ...snapshot([row(1, "delivered")]), identity: replacement }))
      .toThrow(/story identity changed|different project\/story/);
  });
}

test("invalid or duplicate delivery IDs and changed actions are refused", () => {
  const invalidRows: Array<Array<[number, string, string]>> = [
    [row(1, "pending"), row(1, "delivered")],
    [row(0, "delivered")],
    [row(-1, "delivered")],
    [row(1.5, "delivered")],
    [row(Number.MAX_SAFE_INTEGER + 1, "delivered")],
    [[1, "unknown-action", "delivered"]],
  ];
  for (const rows of invalidRows) {
    const barrier = new BlockDeliveryBarrier("alpha-project", "AA-171");
    expect(() => barrier.observe(snapshot(rows))).toThrow(/invalid cleanup delivery identity/);
  }
  const barrier = new BlockDeliveryBarrier("alpha-project", "AA-171");
  barrier.observe(snapshot([row(1, "attempting")]));
  expect(() => barrier.observe(snapshot([[1, "resume", "delivered"]]))).toThrow(/changed action/);
});

/** Writes the private delivery fixture database the reader cases query.
 * The one call site of the audited setup command (tests/e2e_browser_coverage.rs).
 * Bounded by the graced patience sampled now, not a fixed literal (SH-765).
 * It stays synchronous: it runs before anything in the test that needs the
 * event loop. */
function writeDeliveryFixture(testInfo: TestInfo): string {
  const path = testInfo.outputPath("delivery-read-fixture.db");
  mkdirSync(dirname(path), { recursive: true });
  execFileSync("python3", ["-c", `
import sqlite3, sys
with sqlite3.connect(sys.argv[1]) as db:
    db.executescript("""
        CREATE TABLE projects(id,uuid,slug,prefix,checkout_path);
        CREATE TABLE stories(project_id,story_no,created_at);
        CREATE TABLE block_deliveries(id,project_id,story_no,action,status);
        INSERT INTO projects VALUES(1,'fixture-uuid','alpha-project','AA','/fixture'),(2,'other','beta-project','BB','/other');
        INSERT INTO stories VALUES(1,171,'created'),(1,172,'neighbor'),(2,171,'other');
        INSERT INTO block_deliveries VALUES(1,1,171,'interrupt','attempting'),(2,1,172,'interrupt','pending'),(3,2,171,'interrupt','pending');
    """)
`, path], { timeout: gracedPatience(), stdio: "pipe" });
  return path;
}

/** The python3 an unshimmed PATH resolves, for the shim to hand over to. */
function realPython3(): string {
  for (const dir of (process.env.PATH ?? "").split(delimiter)) {
    const candidate = join(dir, "python3");
    try {
      accessSync(candidate, constants.X_OK);
      return candidate;
    } catch {
      // Not executable here; PATH order decides, so try the next entry.
    }
  }
  throw new Error("python3 is not on PATH, so the barrier cannot run at all");
}

/**
 * Runs `body` with a `python3` first on PATH that records each start, waits
 * `delayMs`, and then execs the real interpreter with the original argv.
 * `/bin/sh` execs Python for the wait, so the whole start is one process:
 * a bound that kills it closes its pipes at once. A shell `sleep` child
 * would hold them open after the kill and hide the bound.
 */
async function withSlowPython3<T>(
  testInfo: TestInfo,
  delayMs: number,
  body: (startsLog: string) => Promise<T>,
): Promise<T> {
  const real = realPython3();
  const dir = testInfo.outputPath("python3-shim");
  const startsLog = join(dir, "starts");
  for (const path of [real, startsLog]) {
    if (path.includes("'")) throw new Error(`the shim cannot quote ${path}`);
  }
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, "python3"), [
    "#!/bin/sh",
    `printf 'start\\n' >> '${startsLog}'`,
    `exec '${real}' -c 'import os, sys, time; time.sleep(float(sys.argv[1])); os.execv(sys.argv[2], sys.argv[2:])' '${delayMs / 1000}' '${real}' "$@"`,
    "",
  ].join("\n"), { mode: 0o755 });
  if (pathBeforeShim !== null) throw new Error("a python3 shim is already first on PATH");
  pathBeforeShim = process.env.PATH;
  process.env.PATH = `${dir}${delimiter}${pathBeforeShim ?? ""}`;
  try {
    return await body(startsLog);
  } finally {
    process.env.PATH = pathBeforeShim;
    pathBeforeShim = null;
  }
}

/** Creates one ordinary story in Alpha through the API and returns its ID. */
async function createAlphaStory(request: APIRequestContext, title: string): Promise<string> {
  const before = new Set((await storiesInProject(request, "Alpha Project")).map((s) => s.id));
  const slug = await projectSlug(request, "Alpha Project");
  const created = await request.post(fixtureApiUrl(`/api/repos/${encodeURIComponent(slug)}/story`), {
    headers: { "X-Storyhook": "1", "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN") },
    data: { title },
  });
  expect(created.ok(), await created.text()).toBe(true);
  const added = (await storiesInProject(request, "Alpha Project")).filter((s) => !before.has(s.id));
  expect(added, "exactly one new story in Alpha").toHaveLength(1);
  return added[0].id;
}

/** One second past the fixed 5 s bound the barrier read had before SH-765. */
const PAST_THE_OLD_BOUND_MS = BASE_EXPECT_TIMEOUT_MS + 1_000;

/** A patience short enough that the shimmed read cannot finish inside it. */
const SHORT_PATIENCE_MS = 1_000;

test("the snapshot reader refuses absent or relative store paths", async () => {
  for (const path of [undefined, null, "", "relative-store.db"]) {
    await expect(readBlockDeliverySnapshot(path, "alpha-project", "AA-171", gracedPatience()))
      .rejects.toThrow(/absolute isolated STORYHOOK_STORE_PATH/);
  }
});

test("the snapshot reader selects the exact story and never creates a missing store", async ({}, testInfo) => {
  const path = writeDeliveryFixture(testInfo);
  const before = readFileSync(path);
  await expect(readBlockDeliverySnapshot(path, "alpha-project", "AA-171", gracedPatience()))
    .resolves.toEqual(snapshot([row(1, "attempting")]));
  expect(readFileSync(path)).toEqual(before);
  await expect(readBlockDeliverySnapshot(path, "alpha-project", "BB-171", gracedPatience()))
    .rejects.toThrow(/story identity is absent/);
  await expect(readBlockDeliverySnapshot(`${path}.missing`, "alpha-project", "AA-171", gracedPatience()))
    .rejects.toThrow();
  expect(existsSync(`${path}.missing`)).toBe(false);
});

test("the snapshot reader refuses an unusable bound before it starts python3", async ({}, testInfo) => {
  const path = writeDeliveryFixture(testInfo);
  await withSlowPython3(testInfo, 0, async (startsLog) => {
    // Node reads a timeout of 0 as "no bound at all".
    for (const bound of [undefined, null, 0, -1, 1.5, Number.NaN, Number.POSITIVE_INFINITY, "5000"]) {
      await expect(
        readBlockDeliverySnapshot(path, "alpha-project", "AA-171", bound as unknown as number),
        `bound ${String(bound)}`,
      ).rejects.toThrow(/positive whole number of milliseconds/);
    }
    expect(existsSync(startsLog), "no refused bound may start python3").toBe(false);
    // Control: the shim is first on PATH, so the absence above is evidence.
    await expect(readBlockDeliverySnapshot(path, "alpha-project", "AA-171", gracedPatience()))
      .resolves.toEqual(snapshot([row(1, "attempting")]));
    expect(readFileSync(startsLog, "utf8")).toBe("start\n");
  });
});

// SH-765: a python3 start slower than the old fixed bound failed the whole
// cleanup under load and stranded the story for later specs to count.
test("cleanup removes a story whose barrier read is slower than the old fixed bound", async ({ request }, testInfo) => {
  test.setTimeout(testInfo.timeout + PAST_THE_OLD_BOUND_MS);
  const baseline = new Set((await storiesInProject(request, "Alpha Project")).map((s) => s.id));
  const id = await createAlphaStory(request, "SH-765 slow barrier read");
  await withSlowPython3(testInfo, PAST_THE_OLD_BOUND_MS, async () => {
    await removeStrays(request, "Alpha Project", baseline, PAST_THE_OLD_BOUND_MS + gracedPatience());
  });
  expect((await storiesInProject(request, "Alpha Project")).map((s) => s.id)).not.toContain(id);
});

test("a barrier read that outlasts its patience fails naming the barrier, and the next worker's heal removes the stray", async ({ request }, testInfo) => {
  const baseline = new Set((await storiesInProject(request, "Alpha Project")).map((s) => s.id));
  const id = await createAlphaStory(request, "SH-765 barrier out of patience");
  const slug = await projectSlug(request, "Alpha Project");
  const shimDelayMs = SHORT_PATIENCE_MS + PAST_THE_OLD_BOUND_MS;
  const started = performance.now();
  await withSlowPython3(testInfo, shimDelayMs, async () => {
    await expect(removeStrays(request, "Alpha Project", baseline, SHORT_PATIENCE_MS))
      .rejects.toThrow(new RegExp(
        `^cleanup barrier: the wait for ${slug}/${id}'s block deliveries ran out of its ` +
          `${SHORT_PATIENCE_MS}ms patience .*No DELETE was sent\\.$`,
      ));
  });
  expect(performance.now() - started, "the patience, not the slow start, ended the wait")
    .toBeLessThan(shimDelayMs);
  expect((await storiesInProject(request, "Alpha Project")).map((s) => s.id)).toContain(id);

  // A failed cleanup fails its test, and Playwright starts a new worker for
  // the next one. That worker's first test runs this heal before any hook.
  await healFixtureProjects(request);
  for (const project of CLEANED_PROJECTS) {
    const present = new Set((await storiesInProject(request, project)).map((s) => s.id));
    expect(present, `${project} is back to the run's baseline`).toEqual(fixtureBaseline(project));
  }
});

// SH-765: the baseline is the run's, captured before any worker, so a worker
// restarted after a failed cleanup cannot take the failure's stray in.
test("the run's fixture baseline names the seeded stories of every cleaned project", async ({ request }) => {
  for (const project of CLEANED_PROJECTS) {
    const baseline = fixtureBaseline(project);
    expect(baseline.size, `${project} was seeded before the run`).toBeGreaterThan(0);
    const present = new Set((await storiesInProject(request, project)).map((s) => s.id));
    for (const id of baseline) expect(present.has(id), `${project} still holds seeded ${id}`).toBe(true);
  }
});

test("the run's fixture baseline is loud when absent, malformed, or missing a project", () => {
  // Synchronous on purpose: no await, so a timeout cannot abandon this body
  // before the finally restores the run's real value.
  const captured = process.env[FIXTURE_BASELINE_ENV];
  try {
    delete process.env[FIXTURE_BASELINE_ENV];
    expect(() => fixtureBaseline("Alpha Project")).toThrow(/E2E_FIXTURE_BASELINE is not set/);
    for (const [raw, refusal] of [
      ["{", /is not JSON/],
      ["null", /not a map of project names/],
      ["[]", /not a map of project names/],
      ['{"Alpha Project":"AA-1"}', /not a map of project names/],
      ['{"Alpha Project":[1]}', /not a map of project names/],
    ] as const) {
      process.env[FIXTURE_BASELINE_ENV] = raw;
      expect(() => fixtureBaseline("Alpha Project"), raw).toThrow(refusal);
    }
    process.env[FIXTURE_BASELINE_ENV] = '{"Alpha Project":["AA-1"]}';
    expect(() => fixtureBaseline("Beta Project")).toThrow(/no project named "Beta Project"; it has "Alpha Project"/);
    expect(() => fixtureBaseline("constructor")).toThrow(/no project named "constructor"/);
    expect([...fixtureBaseline("Alpha Project")]).toEqual(["AA-1"]);
  } finally {
    if (captured === undefined) delete process.env[FIXTURE_BASELINE_ENV];
    else process.env[FIXTURE_BASELINE_ENV] = captured;
  }
});

// SH-765 / D7c: a heal that cannot remove a stray fails one test, not every
// remaining test. The marker lives in this test's own output directory here;
// the fixture uses the project's, which Playwright clears at each run's start.
test("a failed heal fails once, and later workers report it without retrying", async ({}, testInfo) => {
  const markerDir = testInfo.outputPath("heal-marker");
  const attempts: string[] = [];
  const failingHeal = async () => {
    attempts.push("heal");
    throw new Error("DELETE AA-9 answered 409");
  };
  const first = { title: "first test of worker 2", annotations: [] as TestInfo["annotations"] };
  await expect(healAtWorkerStart(markerDir, first, failingHeal))
    .rejects.toThrow(/^fixture-heal: a new worker could not remove .*Later workers will not retry/);
  expect(attempts).toEqual(["heal"]);

  const later = { title: "first test of worker 3", annotations: [] as TestInfo["annotations"] };
  await healAtWorkerStart(markerDir, later, failingHeal);
  expect(attempts, "a later worker does not retry a failed heal").toEqual(["heal"]);
  expect(later.annotations).toEqual([{
    type: "fixture-heal",
    description: expect.stringMatching(
      /^fixture-heal: not retried; .*before "first test of worker 2": DELETE AA-9 answered 409$/,
    ),
  }]);
});

test("a heal runs when no earlier worker's heal failed", async ({}, testInfo) => {
  const attempts: string[] = [];
  await healAtWorkerStart(testInfo.outputPath("heal-marker"), { title: "t", annotations: [] }, async () => {
    attempts.push("heal");
  });
  expect(attempts).toEqual(["heal"]);
});
