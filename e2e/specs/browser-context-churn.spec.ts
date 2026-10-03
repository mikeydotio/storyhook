import { gracedPatience } from "../load-grace";
import { test, expect } from "./support";

/** Existing whole-sweep allowance; the test watchdog owns its load grace. */
const CONTEXT_SWEEP_BASE_MS = 120_000;

/**
 * SH-588: WebKit 2336 wedges a fresh page after roughly 65 navigations in
 * one browser process on macOS. No request reaches the server. Exercise
 * two such lifetimes with one browser and distinct contexts, so a dependency
 * regression cannot hide in short specs. A fixture document isolates browser
 * navigation from the dashboard's persistent SSE connections.
 * Upstream: https://github.com/microsoft/playwright/issues/42385.
 */
test("one browser keeps navigating across 128 fresh contexts", async ({ browser }) => {
  test.setTimeout(CONTEXT_SWEEP_BASE_MS);
  for (let iteration = 0; iteration < 128; iteration++) {
    await test.step(`context ${iteration + 1}`, async () => {
      const context = await browser.newContext();
      try {
        const page = await context.newPage();
        await page.route("http://navigation.test/", (route) => route.fulfill({
          contentType: "text/html",
          body: "<!doctype html><title>Navigation probe</title><h1>Ready</h1>",
        }));
        // The failure issues no request and never recovers. Bound each probe
        // separately so its diagnostic identifies the wedged context.
        const response = await page.goto("http://navigation.test/", { timeout: gracedPatience() });
        expect(response?.status()).toBe(200);
        await expect(page.getByRole("heading", { name: "Ready" })).toBeVisible();
      } finally {
        await context.close();
      }
    });
  }
});
