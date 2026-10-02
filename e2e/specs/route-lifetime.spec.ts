import { expect, latch, requiredEnv, seedToken, test } from "./support";
import { withDrainedRoutes } from "../route-lifetime";

for (const fails of [false, true]) {
  test(`response routes finish before their scope exits after body ${fails ? "failure" : "success"}`, async ({ page }) => {
    await seedToken(page);
    await page.goto("/");
    const fetched = latch();
    const release = latch();
    const bodyReturned = latch();
    const sentinel = new Error("scope body sentinel");
    let rewritten: unknown;
    let fetchPromise: Promise<unknown> | undefined;
    let settled = false;

    const outcome = withDrainedRoutes(page, async () => {
      await page.route("**/api/repos?sh813-route-lifetime", async (route) => {
        const response = await route.fetch({
          headers: { ...route.request().headers(), "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN") },
        });
        fetched.release();
        // This is the observed race: the response exists, but its body is
        // read after the test body ends. No product behavior is replaced.
        await release.held;
        rewritten = await response.json();
        await route.fulfill({ response, json: rewritten });
      });
      fetchPromise = page.evaluate(() => fetch("/api/repos?sh813-route-lifetime").then((response) => response.json()));
      await fetched.held;
      bodyReturned.release();
      if (fails) throw sentinel;
      return "scope value";
    }).then(
      (value) => { settled = true; return { kind: "value" as const, value }; },
      (error: unknown) => { settled = true; return { kind: "error" as const, error }; },
    );

    try {
      await bodyReturned.held;
      // Flush completion microtasks on the Node event loop. The native route
      // remains held indefinitely, so this is an ordering proof, not a sleep.
      await new Promise<void>((resolve) => setImmediate(resolve));
      expect(settled, "the page owner must still be waiting for its response handler").toBe(false);
    } finally {
      release.release();
      await outcome;
      await fetchPromise;
    }

    expect(Array.isArray(rewritten)).toBe(true);
    const result = await outcome;
    if (fails) expect(result).toEqual({ kind: "error", error: sentinel });
    else expect(result).toEqual({ kind: "value", value: "scope value" });
  });
}
