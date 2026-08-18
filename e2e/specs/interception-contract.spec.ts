import { expect, test } from "./support";
import { contention, cores } from "../load-grace";
import {
  heldReadDeadlineMs,
  holdUntilRefused,
  projectSlug,
  requiredEnv,
  seedToken,
} from "./support";

/**
 * SH-347 investigation: three engine facts every hold-based helper in
 * `support.ts` depends on (`holdUntilRefused`, `holdFetch`,
 * `holdDetailFetch`), measured directly against a raw page-issued
 * `XMLHttpRequest` rather than assumed from a hypothesis about "WebKit".
 *
 * Scratch for now -- these probes exist to settle which of SH-347's five
 * candidate mechanisms (M1-M5, see the plan comment on the story) is
 * actually in play before any fix is written. Each annotation is the
 * evidence; nothing here asserts a verdict yet.
 */

test.describe("SH-347 interception-contract probes", () => {
  test.beforeEach(async ({ page }) => {
    await seedToken(page);
    await page.goto("/");
  });

  test("probe A: is the page's XHR timeout armed while a route is held indefinitely?", async ({
    page,
    browserName,
  }) => {
    await page.route("**/probe/a", async () => {
      // Never resolves. This route is held forever -- the shape
      // `holdUntilRefused()` and `holdFetch()` both start every request in.
      await new Promise(() => {});
    });

    const result = await page.evaluate(() => {
      return new Promise<{ fired: "timeout" | "error" | "none"; elapsedMs: number }>(
        (resolve) => {
          const start = Date.now();
          const xhr = new XMLHttpRequest();
          xhr.open("GET", "/probe/a", true);
          xhr.timeout = 1000;
          xhr.ontimeout = () => resolve({ fired: "timeout", elapsedMs: Date.now() - start });
          xhr.onerror = () => resolve({ fired: "error", elapsedMs: Date.now() - start });
          xhr.send();
          // Bounded independently of xhr.timeout, so a WebKit that never
          // arms the clock can't hang this probe out to the suite's own
          // test timeout -- a "none" verdict at ~4s is itself the finding.
          setTimeout(() => resolve({ fired: "none", elapsedMs: Date.now() - start }), 4000);
        },
      );
    });

    test.info().annotations.push({
      type: "sh347-probe-a",
      description: `${browserName}: xhr.timeout=1000ms, fired=${result.fired} at ${result.elapsedMs}ms`,
    });

    // Recorded, not asserted strictly -- a WebKit-only "none" here is the
    // headline result (M5 confirmed), not a bug in the probe.
    expect(result.elapsedMs).toBeLessThan(4100);
  });

  test("probe B: how long after route.abort() does the page's XHR see it?", async ({
    page,
    browserName,
  }) => {
    let release: () => void = () => {};
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    await page.route("**/probe/b", async (route) => {
      await held;
      await route.abort();
    });

    const resultPromise = page.evaluate(() => {
      return new Promise<{ fired: "error" | "timeout"; elapsedFromOpenMs: number }>(
        (resolve) => {
          const start = Date.now();
          const xhr = new XMLHttpRequest();
          xhr.open("GET", "/probe/b", true);
          // Deliberately far past anything this probe waits -- if THIS is
          // what settles the request instead of the abort, that is M4.
          xhr.timeout = 60_000;
          xhr.onerror = () => resolve({ fired: "error", elapsedFromOpenMs: Date.now() - start });
          xhr.ontimeout = () => resolve({ fired: "timeout", elapsedFromOpenMs: Date.now() - start });
          xhr.send();
        },
      );
    });

    // Give the request time to actually reach the held route before
    // releasing it, so the measured delta is abort-to-onerror, not
    // open-to-onerror.
    await new Promise((r) => setTimeout(r, 500));
    const releasedAtMs = Date.now();
    release();
    const result = await resultPromise;

    test.info().annotations.push({
      type: "sh347-probe-b",
      description:
        `${browserName}: released at t+500ms, XHR ${result.fired} at ` +
        `t+${result.elapsedFromOpenMs}ms (~${result.elapsedFromOpenMs - 500}ms after release)`,
    });

    // Near-instant (~500ms + epsilon) refutes M4; anywhere near the 60s
    // xhr.timeout (or "timeout" rather than "error") confirms it.
  });

  test("probe C: does a route.fulfill() delivered after the client abandoned the request throw?", async ({
    page,
    browserName,
  }) => {
    let thrown: string | null = null;

    await page.route(/\/api\/repos$/, async (route) => {
      if (route.request().method() !== "GET") {
        await route.continue();
        return;
      }
      // A real reply, available quickly -- delayed delivery is the point,
      // not a slow backend. `route.fetch()` does not carry the HttpOnly
      // auth cookie, so the token header is forced in the way
      // `holdFetch()` (support.ts) already does.
      const response = await route.fetch({
        headers: {
          ...route.request().headers(),
          "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
        },
      });
      // Comfortably past the client's own 50ms abandon point below.
      await new Promise((r) => setTimeout(r, 300));
      try {
        await route.fulfill({ response });
      } catch (e) {
        thrown = String(e);
      }
    });

    const result = await page.evaluate(() => {
      return new Promise<{ fired: "load" | "error" | "timeout"; status: number }>(
        (resolve) => {
          const xhr = new XMLHttpRequest();
          xhr.open("GET", "/api/repos", true);
          xhr.timeout = 50;
          xhr.ontimeout = () => resolve({ fired: "timeout", status: 0 });
          xhr.onerror = () => resolve({ fired: "error", status: 0 });
          xhr.onload = () => resolve({ fired: "load", status: xhr.status });
          xhr.send();
        },
      );
    });

    // Let the delayed route handler actually settle before the test (and
    // its fixture teardown) tears the page down underneath it.
    await new Promise((r) => setTimeout(r, 500));

    test.info().annotations.push({
      type: "sh347-probe-c",
      description:
        `${browserName}: client saw ${result.fired} (status ${result.status}) at its own ` +
        `50ms deadline; ` +
        (thrown
          ? `the route's own late route.fulfill() THREW: ${thrown}`
          : "the route's own late route.fulfill() completed without throwing"),
    });
  });

  /**
   * Council-decided Probe D (verdict recorded on SH-347's own comments):
   * measures abort-delivery timing under the SAME real conditions that
   * produced `board-readiness.spec.ts`'s original WebKit flakes -- a held
   * `/data` route plus the actual inter-step work between the hold and
   * `refuse()` (a navigation, a board render, a click, a popover open) --
   * rather than a synthetic delay. No synthetic load campaign is needed:
   * every original flake was observed "in a full-suite run, never in
   * isolation," so the load this probe needs is exactly the load the
   * suite's own WebKit leg already provides when this file runs as part of
   * it.
   *
   * Never throws: a bounded poll, not an `expect()`, so this probe can only
   * ever produce evidence, never fail the gate on its own account -- the
   * decision these measurements feed (lifting or permanently re-reasoning
   * `board-readiness.spec.ts`'s three quarantines) is a separate, later step.
   */
  const PROBE_D_POLL_BOUND_MS = 12_000;

  async function pollForText(
    locator: import("@playwright/test").Locator,
    expectedText: string,
    boundMs: number,
  ): Promise<{ seen: boolean; elapsedMs: number }> {
    const start = Date.now();
    while (Date.now() - start < boundMs) {
      const text = await locator.textContent().catch(() => null);
      if (text === expectedText) {
        return { seen: true, elapsedMs: Date.now() - start };
      }
      await new Promise((r) => setTimeout(r, 50));
    }
    return { seen: false, elapsedMs: Date.now() - start };
  }

  test("probe D1: abort-delivery timing under board-readiness's own real inter-step work, single route (SH-347)", async ({
    page,
    request,
    browserName,
  }) => {
    const slug = await projectSlug(request, "Alpha Project");
    const data = await holdUntilRefused(
      page,
      (url) => url.pathname === `/api/repos/${encodeURIComponent(slug)}/data`,
    );

    const beforeNav = Date.now();
    await page.goto(
      `/?project=${encodeURIComponent(slug)}&boardFetchTimeoutMs=${heldReadDeadlineMs()}`,
    );
    await expect(page.locator("#board-view")).toBeVisible();
    await page.locator("#drafts-btn").click();
    const beforeRefuseElapsedMs = Date.now() - beforeNav;

    data.refuse();
    const outcome = await pollForText(
      page.locator("#drafts-list"),
      "Couldn't load this project's drafts. Retrying…",
      PROBE_D_POLL_BOUND_MS,
    );

    test.info().annotations.push({
      type: "sh347-probe-d1",
      description:
        `${browserName}: real inter-step work before refuse() took ` +
        `${beforeRefuseElapsedMs}ms; page reflected the abort ` +
        `${outcome.seen ? `${outcome.elapsedMs}ms after refuse()` : `NOT within ${PROBE_D_POLL_BOUND_MS}ms of refuse()`} ` +
        `(contention=${contention().toFixed(2)}, cores=${cores()})`,
    });
  });

  test("probe D2: abort-delivery timing with a second route held open throughout (SH-347)", async ({
    page,
    request,
    browserName,
  }) => {
    const alpha = await projectSlug(request, "Alpha Project");
    const beta = await projectSlug(request, "Beta Project");
    const alphaData = await holdUntilRefused(
      page,
      (url) => url.pathname === `/api/repos/${encodeURIComponent(alpha)}/data`,
    );
    // Beta's read is held and never refused, mirroring
    // `board-readiness.spec.ts`'s own third quarantined test exactly.
    await holdUntilRefused(
      page,
      (url) => url.pathname === `/api/repos/${encodeURIComponent(beta)}/data`,
    );

    const beforeNav = Date.now();
    await page.goto(
      `/?project=${encodeURIComponent(alpha)}&boardFetchTimeoutMs=${heldReadDeadlineMs()}`,
    );
    await expect(page.locator("#board-view")).toBeVisible();
    await page.locator("#drafts-btn").click();
    const beforeRefuseElapsedMs = Date.now() - beforeNav;

    alphaData.refuse();
    const outcome = await pollForText(
      page.locator("#drafts-list"),
      "Couldn't load this project's drafts. Retrying…",
      PROBE_D_POLL_BOUND_MS,
    );

    test.info().annotations.push({
      type: "sh347-probe-d2",
      description:
        `${browserName}: real inter-step work before refuse() took ` +
        `${beforeRefuseElapsedMs}ms; page reflected the abort ` +
        `${outcome.seen ? `${outcome.elapsedMs}ms after refuse()` : `NOT within ${PROBE_D_POLL_BOUND_MS}ms of refuse()`} ` +
        `with a second route (Beta) held open throughout`,
    });
  });
});
