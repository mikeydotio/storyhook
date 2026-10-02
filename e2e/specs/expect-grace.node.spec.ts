import { test, expect } from "./support";
import { withAssertionGrace, reportAssertionGrace } from "../expect-grace";
import { BASE_EXPECT_TIMEOUT_MS, gracedPatience } from "../load-grace";

/** Observes the timeout Playwright actually passes to a custom matcher. */
const observingExpect = expect.extend({
  toUseBudget(actual: number) {
    return { pass: this.timeout === actual, message: () => `budget ${this.timeout}, expected ${actual}` };
  },
});

test("resamples defaults after construction, including saved soft and poll functions", async () => {
  let ratio = 0.3;
  let samples = 0;
  const checked = withAssertionGrace(observingExpect, () => { samples++; return gracedPatience(ratio); });
  const soft = checked.soft;
  const poll = checked.poll;
  expect(samples).toBe(0);
  checked(BASE_EXPECT_TIMEOUT_MS).toUseBudget();
  for (const value of [8.7, 12, 2, 0.3]) {
    ratio = value;
    const budget = gracedPatience(ratio);
    checked(budget).toUseBudget();
    soft(budget).toUseBudget();
    await poll(() => budget).toUseBudget();
  }
  expect(samples).toBe(13);
});

test("configured timeouts are fixed, while message and soft configurations remain dynamic", () => {
  let budget = BASE_EXPECT_TIMEOUT_MS;
  const checked = withAssertionGrace(observingExpect, () => budget);
  const fixed = checked.configure({ timeout: 71 });
  const zero = fixed.configure({ timeout: 0 });
  const dynamic = fixed.configure({ timeout: undefined, message: "fresh" });
  const soft = checked.configure({ soft: true });
  budget *= 2;
  fixed(71).toUseBudget();
  fixed.configure({ message: "still fixed" })(71).toUseBudget();
  zero(0).toUseBudget();
  dynamic(budget).toUseBudget();
  soft(budget).toUseBudget();
  expect(() => checked.configure({ message: "custom failure" })(1).toBe(2)).toThrow(/custom failure/);
});

test("extensions, promises, asymmetric matchers and negation preserve native behavior", async () => {
  let budget = BASE_EXPECT_TIMEOUT_MS;
  const checked = withAssertionGrace(expect, () => budget).extend({
    toUseBudget(actual: number) {
      return { pass: this.timeout === actual, message: () => `budget ${this.timeout}, expected ${actual}` };
    },
  });
  budget *= 3;
  checked(budget).toUseBudget();
  checked(1).not.toUseBudget();
  await checked(Promise.resolve(budget)).resolves.toUseBudget();
  await checked(Promise.reject(new Error("sentinel"))).rejects.toThrow("sentinel");
  checked({ a: "ok" }).toEqual(checked.objectContaining({ a: checked.any(String) }));
  checked([1]).toEqual(checked.not.arrayContaining([2]));
});

test("disabled grace and extreme load preserve the policy limits", () => {
  const prior = process.env.E2E_LOAD_GRACE;
  try {
    delete process.env.E2E_LOAD_GRACE;
    const checked = withAssertionGrace(observingExpect, () => gracedPatience(1e6));
    checked(BASE_EXPECT_TIMEOUT_MS * 60).toUseBudget();
    process.env.E2E_LOAD_GRACE = "0";
    checked(BASE_EXPECT_TIMEOUT_MS).toUseBudget();
  } finally {
    if (prior === undefined) delete process.env.E2E_LOAD_GRACE;
    else process.env.E2E_LOAD_GRACE = prior;
  }
});

test("default diagnostics are deduplicated per test and omit idle budgets", () => {
  const first = { annotations: [] };
  const second = { annotations: [] };
  const lines: string[] = [];
  const write = (line: string) => { lines.push(line); };
  for (const budget of [5000, 10000, 10000, 43500, 5000, 10000]) {
    reportAssertionGrace(first, budget, write);
  }
  reportAssertionGrace(second, 10000, write);
  expect(first.annotations).toHaveLength(2);
  expect(second.annotations).toHaveLength(1);
  expect(lines).toHaveLength(3);
  expect(lines[1]).toMatch(/default=43500ms.*base=5000ms/);
});

test("poll receives a fresh default and preserves explicit deadlines", async () => {
  let budget = BASE_EXPECT_TIMEOUT_MS;
  const checked = withAssertionGrace(expect, () => budget);
  budget *= 2;
  const started = performance.now();
  const becomesReadyAt = BASE_EXPECT_TIMEOUT_MS * 1.2;
  await checked.poll(() => performance.now() - started >= becomesReadyAt).toBe(true);
  // This deadline proves precedence, rather than offering harness patience.
  const PROOF_TIMEOUT_MS = BASE_EXPECT_TIMEOUT_MS / 50;
  await expect(checked.poll(() => false, { timeout: PROOF_TIMEOUT_MS }).toBe(true))
    .rejects.toThrow(`Timeout ${PROOF_TIMEOUT_MS}ms`);
  await checked.poll(() => true, { timeout: 0 }).toBe(true);
});

test("toPass retains its independent default even with a short configured expect", async () => {
  const PROOF_TIMEOUT_MS = BASE_EXPECT_TIMEOUT_MS / 50;
  const started = performance.now();
  const checked = withAssertionGrace(expect, () => PROOF_TIMEOUT_MS);
  await checked(() => {
    expect(performance.now() - started).toBeGreaterThan(PROOF_TIMEOUT_MS * 2);
  }).toPass();
});

test("soft assertions record failure and continue", async ({}, testInfo) => {
  test.fail();
  const checked = withAssertionGrace(expect, () => BASE_EXPECT_TIMEOUT_MS);
  checked.soft(1, "soft sentinel").toBe(2);
  expect(testInfo.errors).toHaveLength(1);
  expect(testInfo.errors[0].message).toContain("soft sentinel");
});
