import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import type { FullConfig, FullResult, Suite, TestCase, TestResult } from "@playwright/test/reporter";
import SliceReporter from "../slice-reporter";
import { expect, test } from "./support";

test("slice receipts preserve counts, durations and skipped results", async () => {
  const root = mkdtempSync("/tmp/slice-reporter-");
  const names = ["E2E_SLICE_EXPECTED", "E2E_SLICE_REPORT"] as const;
  const saved = names.map(name => process.env[name]);
  try {
    const file = join(root, "fixture.spec.ts");
    const expected = join(root, "expected.tsv");
    const output = join(root, "report.json");
    writeFileSync(file, "");
    writeFileSync(expected, "fixture\tfixture.spec.ts\t3\n");
    process.env.E2E_SLICE_EXPECTED = expected;
    process.env.E2E_SLICE_REPORT = output;
    const item = { expectedStatus: "passed", location: { file }, parent: { project: () => ({ name: "fixture" }) } } as TestCase;
    const declaredFailure = { ...item, expectedStatus: "failed" } as TestCase;
    const reporter = new SliceReporter();
    reporter.onBegin({ rootDir: root } as FullConfig, { allTests: () => [item, item, declaredFailure] } as Suite);
    reporter.onTestEnd(item, { status: "passed", duration: 1200, retry: 0 } as TestResult);
    reporter.onTestEnd(item, { status: "skipped", duration: 0, retry: 0 } as TestResult);
    reporter.onTestEnd(declaredFailure, { status: "failed", duration: 800, retry: 0 } as TestResult);
    expect(await reporter.onEnd({ status: "passed" } as FullResult)).toBeUndefined();
    const receipt = JSON.parse(readFileSync(output, "utf8"));
    expect(receipt.status).toBe("passed");
    expect(receipt.errors).toEqual([]);
    expect(receipt.files).toEqual([{ project: "fixture", file: "fixture.spec.ts", count: 3,
      completed: 3, passed: 2, skipped: 1, retries: 0, seconds: 2 }]);
    // A stale plan is a failure even when the runner offers a passed result.
    writeFileSync(expected, "fixture\tfixture.spec.ts\t3\n");
    const mismatch = new SliceReporter();
    mismatch.onBegin({ rootDir: root } as FullConfig, { allTests: () => [item, item] } as Suite);
    expect(await mismatch.onEnd({ status: "passed" } as FullResult)).toEqual({ status: "failed" });
  } finally {
    names.forEach((name, index) => {
      if (saved[index] === undefined) delete process.env[name];
      else process.env[name] = saved[index];
    });
    rmSync(root, { recursive: true, force: true });
  }
});
