import { expect, test } from "./support";
import { request as playwrightRequest } from "@playwright/test";
import { createServer } from "node:http";
import { setTimeout as delay } from "node:timers/promises";
import { BASE_REQUEST_TIMEOUT_MS, gracedRequestBudget } from "../fixture-api";
import {
  BASE_EXPECT_TIMEOUT_MS,
  BASE_TEST_TIMEOUT_MS,
  MAX_GRACE_MULTIPLIER,
  MAX_TEST_TIMEOUT_MS,
  gracedBudget,
  gracedOperationBudget,
  gracedPatience,
  gracedTestBudget,
  graceMultiplier,
  resetTestBudget,
} from "../load-grace";

/**
 * Executed control for `e2e/load-grace.ts`'s pure functions (SH-347).
 * `tests/e2e_load_grace.rs` can only confirm the wiring is present -- this
 * is what actually runs the policy and checks its arithmetic, at ratios no
 * real run of this suite is guaranteed to ever produce (an idle developer
 * machine may never see contention &gt; 1, and nobody wants to wait for one
 * that does just to prove the ceiling clamps). No `page` fixture: these are
 * plain function calls, so this file never launches a browser.
 */

test.describe("load-grace pure functions", () => {
  test("operation patience preserves idle bases, samples grace and clamps large bases", () => {
    const original = process.env.E2E_LOAD_GRACE;
    try {
      delete process.env.E2E_LOAD_GRACE;
      for (const base of [1_000, 5_000, 8_000, 10_000, 15_000, 20_000, 30_000, 45_000]) {
        for (const ratio of [0, 0.3, 1]) expect(gracedOperationBudget(base, ratio)).toBe(base);
        expect(gracedOperationBudget(base, 2)).toBe(base * 2);
        expect(gracedOperationBudget(base, 1_000_000)).toBe(Math.min(MAX_TEST_TIMEOUT_MS, base * MAX_GRACE_MULTIPLIER));
      }
      for (const ratio of [0, 2, 100]) {
        expect(gracedPatience(ratio)).toBe(gracedBudget(BASE_EXPECT_TIMEOUT_MS, ratio));
      }
      expect(BASE_REQUEST_TIMEOUT_MS).toBe(30_000);
      expect(gracedRequestBudget(0)).toBe(BASE_REQUEST_TIMEOUT_MS);
      expect(gracedRequestBudget(2)).toBe(BASE_REQUEST_TIMEOUT_MS * 2);
      expect(gracedRequestBudget(100)).toBe(MAX_TEST_TIMEOUT_MS);
      process.env.E2E_LOAD_GRACE = "0";
      expect(gracedOperationBudget(45_000, 100)).toBe(45_000);
      expect(gracedPatience(100)).toBe(BASE_EXPECT_TIMEOUT_MS);
      expect(gracedRequestBudget(100)).toBe(BASE_REQUEST_TIMEOUT_MS);
    } finally {
      if (original === undefined) delete process.env.E2E_LOAD_GRACE;
      else process.env.E2E_LOAD_GRACE = original;
    }
  });
  test("idle contention (ratio <= 1) leaves both config budgets bit-identical to SH-222's own numbers", () => {
    for (const ratio of [0, 0.3, 0.94, 1]) {
      expect(graceMultiplier(ratio)).toBe(1);
      expect(gracedBudget(BASE_TEST_TIMEOUT_MS, ratio)).toBe(BASE_TEST_TIMEOUT_MS);
      expect(gracedBudget(BASE_EXPECT_TIMEOUT_MS, ratio)).toBe(BASE_EXPECT_TIMEOUT_MS);
    }
  });

  test("above the point of contention, the multiplier tracks the ratio", () => {
    expect(graceMultiplier(2)).toBe(2);
    expect(graceMultiplier(4.4)).toBeCloseTo(4.4, 6);
    expect(gracedBudget(BASE_TEST_TIMEOUT_MS, 2)).toBe(BASE_TEST_TIMEOUT_MS * 2);
  });

  test("graceMultiplier is monotone non-decreasing in the contention ratio", () => {
    const ratios = [0, 0.5, 1, 1.5, 2, 5, 10, 30, 59, 60, 61, 100, 1000];
    let previous = -Infinity;
    for (const ratio of ratios) {
      const multiplier = graceMultiplier(ratio);
      expect(multiplier).toBeGreaterThanOrEqual(previous);
      previous = multiplier;
    }
  });

  test("gracedBudget never returns below its own base -- grace only ever adds patience", () => {
    for (const ratio of [0, 0.5, 1, 3, 100]) {
      expect(gracedBudget(BASE_TEST_TIMEOUT_MS, ratio)).toBeGreaterThanOrEqual(BASE_TEST_TIMEOUT_MS);
    }
  });

  test("the multiplier clamps at MAX_GRACE_MULTIPLIER, so BASE_TEST_TIMEOUT_MS graced at any ratio never exceeds MAX_TEST_TIMEOUT_MS", () => {
    expect(graceMultiplier(1_000_000)).toBe(MAX_GRACE_MULTIPLIER);
    expect(gracedBudget(BASE_TEST_TIMEOUT_MS, 1_000_000)).toBe(MAX_TEST_TIMEOUT_MS);
  });

  test("gracedTestBudget clamps to the ABSOLUTE ceiling regardless of its base -- unlike gracedBudget, which clamps the multiplier", () => {
    // A spec-set custom base (e.g. dispatch.spec.ts's own multiple of
    // DISPATCH_COMPLETION_TIMEOUT) must never be stretched past the user's
    // literal 15-minute ceiling just because it started larger than
    // BASE_TEST_TIMEOUT_MS -- this is the exact bug the watchdog's design
    // doc warns gracedBudget would have if it were used here instead.
    const customBase = 120_000; // dispatch.spec.ts's own AC2 budget
    expect(gracedTestBudget(customBase, 1_000_000)).toBe(MAX_TEST_TIMEOUT_MS);
    expect(gracedTestBudget(customBase, 1_000_000)).toBeLessThan(
      customBase * MAX_GRACE_MULTIPLIER,
    );
  });

  test("gracedTestBudget at idle returns its base unchanged", () => {
    expect(gracedTestBudget(BASE_TEST_TIMEOUT_MS, 0.3)).toBe(BASE_TEST_TIMEOUT_MS);
    expect(gracedTestBudget(120_000, 0.5)).toBe(120_000);
  });

  test("resetTestBudget resets from now without crossing the absolute wall-clock ceiling", () => {
    expect(resetTestBudget(BASE_TEST_TIMEOUT_MS, 20_000, 2)).toBe(30_000);
    expect(resetTestBudget(BASE_TEST_TIMEOUT_MS, MAX_TEST_TIMEOUT_MS - 1_000, 2)).toBe(1_000);
    expect(resetTestBudget(BASE_TEST_TIMEOUT_MS, MAX_TEST_TIMEOUT_MS + 1_000, 2)).toBe(1);
  });

  test("MAX_TEST_TIMEOUT_MS is exactly the user's own 15-minute determination", () => {
    expect(MAX_TEST_TIMEOUT_MS).toBe(15 * 60 * 1000);
  });
});

/** An intentionally short context default; the explicit request budget must
 * override it. The delayed endpoint crosses that default by construction. */
const SHORT_REQUEST_PROOF_MS = 100;

test("explicit request patience overrides the context default and still bounds a held endpoint", async () => {
  const server = createServer(async (req, res) => {
    if (req.url === "/held") return;
    await delay(SHORT_REQUEST_PROOF_MS * 3);
    res.end("ready");
  });
  await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (address === null || typeof address === "string") throw new Error("missing fixture server address");
  const api = await playwrightRequest.newContext({ timeout: SHORT_REQUEST_PROOF_MS });
  try {
    const url = `http://127.0.0.1:${address.port}`;
    const response = await api.get(`${url}/delayed`, { timeout: gracedRequestBudget() });
    expect(await response.text()).toBe("ready");
    await expect(api.get(`${url}/held`, {
      timeout: gracedOperationBudget(SHORT_REQUEST_PROOF_MS, 1),
    })).rejects.toThrow(/Timeout.*exceeded/);
  } finally {
    await api.dispose();
    server.closeAllConnections();
    await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
  }
});
