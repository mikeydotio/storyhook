import { expect, test } from "./support";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { reporterBudget, runReporterTests } from "../reporter-command";
import { BASE_EXPECT_TIMEOUT_MS, BASE_TEST_TIMEOUT_MS, MAX_TEST_TIMEOUT_MS, gracedPatience } from "../load-grace";

test("browser launch failure interrupts the project while ordinary failures continue", async () => {
  const budget = reporterBudget();
  test.setTimeout(budget.testMs);
  const stderr = await runReporterTests(budget.processMs);
  expect(stderr).toContain("OK");
});

test("reporter budget reserves cleanup at idle, under load and at the ceiling", () => {
  const prior = process.env.E2E_LOAD_GRACE;
  try {
    delete process.env.E2E_LOAD_GRACE;
    const base = 4 * 60_000 + BASE_TEST_TIMEOUT_MS;
    expect(reporterBudget(0)).toEqual({ testMs: base, processMs: base - BASE_EXPECT_TIMEOUT_MS });
    expect(reporterBudget(2)).toEqual({ testMs: base * 2, processMs: (base - BASE_EXPECT_TIMEOUT_MS) * 2 });
    const ceiling = reporterBudget(1_000_000);
    expect(ceiling.testMs).toBe(MAX_TEST_TIMEOUT_MS);
    expect(ceiling.processMs).toBeGreaterThan(0);
    expect(ceiling.processMs).toBeLessThan(ceiling.testMs);
    process.env.E2E_LOAD_GRACE = "0";
    expect(reporterBudget(1_000_000)).toEqual(reporterBudget(0));
  } finally {
    if (prior === undefined) delete process.env.E2E_LOAD_GRACE;
    else process.env.E2E_LOAD_GRACE = prior;
  }
});

test("reporter rejects invalid bounds and prior cancellation before starting", async () => {
  for (const bound of [0, -1, 0.5, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1]) {
    await expect(runReporterTests(bound)).rejects.toThrow("positive integer");
  }
  const controller = new AbortController();
  controller.abort(new Error("cancelled before spawn"));
  await expect(runReporterTests(1, controller.signal)).rejects.toThrow("cancelled before spawn");
});

for (const mode of ["timeout", "abort", "failure", "missing"] as const) {
  test(`reporter ${mode} fails with diagnostics after process close`, async () => {
    const root = mkdtempSync("/tmp/SH-805-reporter-");
    const ready = join(root, "ready");
    const cleaned = join(root, "cleaned");
    const priorPath = process.env.PATH;
    const controller = new AbortController();
    const boundMs = gracedPatience();
    test.setTimeout(boundMs * 3);
    if (mode !== "missing") {
      // A real command endpoint: it exits zero after TERM, which must still fail.
      writeFileSync(join(root, "python3"), `#!${process.execPath}
const fs = require('node:fs');
process.on('SIGTERM', () => {
  fs.writeFileSync(${JSON.stringify(cleaned)}, 'cleanup complete');
  process.exit(0);
});
process.stdout.write('fixture stdout\\n');
process.stderr.write('fixture stderr\\n');
fs.writeFileSync(${JSON.stringify(ready)}, String(process.pid));
${mode === "failure" ? "process.exit(7);" : "setInterval(() => {}, 1000);"}
`, { mode: 0o755 });
    }
    process.env.PATH = root;
    const result = runReporterTests(boundMs, controller.signal).then(
      () => ({ error: null }),
      (error: Error) => ({ error }),
    );
    try {
      if (mode === "abort") {
        await expect.poll(() => existsSync(ready), { timeout: boundMs }).toBe(true);
        controller.abort();
      }
      const { error } = await result;
      expect(error).not.toBeNull();
      expect(error!.message).toContain(`bound=${boundMs}ms`);
      expect(error!.message).toContain("python3 test-browser-launch-reporter.py");
      if (mode === "timeout" || mode === "abort") {
        expect(readFileSync(cleaned, "utf8")).toBe("cleanup complete");
      }
      if (mode === "failure") {
        expect(error!.message).toContain("code=7");
        expect(error!.message).toContain("fixture stdout");
        expect(error!.message).toContain("fixture stderr");
      }
      if (mode === "missing") expect(String(error!.cause)).toContain("ENOENT");
    } finally {
      controller.abort();
      await result;
      if (priorPath === undefined) delete process.env.PATH;
      else process.env.PATH = priorPath;
      rmSync(root, { recursive: true, force: true });
    }
  });
}
