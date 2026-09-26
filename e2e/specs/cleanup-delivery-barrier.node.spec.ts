import { test, expect } from "./support";
import { BlockDeliveryBarrier, readBlockDeliverySnapshot } from "../block-delivery-barrier.cjs";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync } from "node:fs";
import { dirname } from "node:path";

// Unit contracts for the cleanup fixture. These use data snapshots, never a
// mock daemon, delivery worker, or terminal effect. The blocked-drop and
// status-destination browser cases exercise the shared barrier against all of
// those production paths in the same matrix.
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

test("the snapshot reader refuses absent or relative store paths", () => {
  for (const path of [undefined, null, "", "relative-store.db"]) {
    expect(() => readBlockDeliverySnapshot(path, "alpha-project", "AA-171"))
      .toThrow(/absolute isolated STORYHOOK_STORE_PATH/);
  }
});

test("the snapshot reader selects the exact story and never creates a missing store", ({}, testInfo) => {
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
`, path], { timeout: 5_000, stdio: "pipe" });
  const before = readFileSync(path);
  expect(readBlockDeliverySnapshot(path, "alpha-project", "AA-171"))
    .toEqual(snapshot([row(1, "attempting")]));
  expect(readFileSync(path)).toEqual(before);
  expect(() => readBlockDeliverySnapshot(path, "alpha-project", "BB-171")).toThrow(/story identity is absent/);
  expect(() => readBlockDeliverySnapshot(`${path}.missing`, "alpha-project", "AA-171")).toThrow();
  expect(existsSync(`${path}.missing`)).toBe(false);
});
