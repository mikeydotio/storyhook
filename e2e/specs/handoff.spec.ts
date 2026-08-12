import { test, expect } from "@playwright/test";
import type { APIRequestContext, Page } from "@playwright/test";
import { cleanUpCreatedStories, requiredEnv } from "./support";

/**
 * SH-251's one-click dashboard, in a real browser: a tab opened at
 * `#h=<coupon>` spends it for the daemon's token before it issues a single
 * request, and never prompts for anything.
 *
 * **These specs deliberately do NOT call `seedToken`.** Like
 * `loopback-no-token.spec.ts`, the whole claim is about a browser that starts
 * with nothing — a seeded credential would make every assertion here pass for
 * the wrong reason.
 *
 * # What this file cannot prove, and must not pretend to
 *
 * SH-251 was decided by an experiment on a real Chrome: `history.replaceState`
 * does **not** scrub the pre-replacement URL from the on-disk history
 * database, it adds a second row beside it. **No spec in this file can
 * observe that**, because Playwright's bundled chromium writes no History
 * database at all. So nothing here is evidence about what a browser persists.
 *
 * The load-bearing claim — that what persists is worth nothing 120 seconds
 * later — is asserted in `src/api/handoff.rs`'s unit tests against an
 * injected clock, and that is where it belongs. These specs are corroboration
 * of *ordering* only, and they carry a harness guard (`expect(hashes.length)`)
 * that fails if the shim recorded nothing, so a vacuous pass reads as red.
 *
 * If you are here to "add the missing e2e coverage" for browser history:
 * there is none to add. Re-run the experiment by hand against a real browser,
 * as `docs/spec/dashboard-authorization.md` records.
 *
 * # Ordering
 *
 * Each test arms and spends its own coupon and starts from a fresh browser
 * context, so neither depends on the other having run. `workers: 1` hides
 * ordering coupling rather than removing it, so that independence is
 * structural here rather than inherited from the config.
 */

cleanUpCreatedStories("Alpha Project");

/** Arms a one-shot coupon the way `story web open` does. */
async function armCoupon(request: APIRequestContext): Promise<string> {
  const armed = await request.post("/api/v1/handoff", {
    headers: { "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN") },
  });
  if (!armed.ok()) {
    throw new Error(
      `arming a handoff answered ${armed.status()}: ${await armed.text()}`,
    );
  }
  const { coupon } = await armed.json();
  if (typeof coupon !== "string" || !/^[0-9a-f]{32}$/.test(coupon)) {
    throw new Error(`the daemon armed something that is not a coupon: ${coupon}`);
  }
  return coupon;
}

/**
 * Records `location.hash` at the moment each request is *constructed* —
 * `XMLHttpRequest.prototype.open` and the `EventSource` constructor being the
 * only two ways this dashboard talks to the daemon.
 *
 * Construction, not completion: what has to hold is that the fragment was
 * already gone before anything went out, and a completion-time reading would
 * pass even if the strip happened during the round trip.
 *
 * Never assert `location.hash` after load instead. `syncUrl()` rebuilds the
 * URL from `pathname + search` on every render and discards the fragment
 * anyway, so that spelling passes with the entire feature deleted.
 */
async function recordHashAtEachRequest(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const seen: string[] = [];
    (window as unknown as { __hashAtRequest: string[] }).__hashAtRequest = seen;

    const open = XMLHttpRequest.prototype.open;
    XMLHttpRequest.prototype.open = function (
      this: XMLHttpRequest,
      ...args: Parameters<typeof open>
    ) {
      seen.push(window.location.hash);
      return open.apply(this, args);
    };

    const Native = window.EventSource;
    if (Native) {
      const Wrapped = function (url: string, init?: EventSourceInit) {
        seen.push(window.location.hash);
        return new Native(url, init);
      } as unknown as typeof EventSource;
      Wrapped.prototype = Native.prototype;
      window.EventSource = Wrapped;
    }
  });
}

async function hashesAtRequest(page: Page): Promise<string[]> {
  return page.evaluate(
    () => (window as unknown as { __hashAtRequest: string[] }).__hashAtRequest,
  );
}

test("a tab opened with a coupon holds a scoped session, and never the token", async ({
  page,
  request,
}) => {
  const coupon = await armCoupon(request);
  await recordHashAtEachRequest(page);

  await page.goto(`/#h=${coupon}`);

  // The dashboard renders, with nothing typed and nothing seeded.
  await expect(
    page.locator(".repo-card-name", { hasText: "Alpha Project" }),
  ).toBeVisible();
  await expect(page.locator("#token-modal")).not.toHaveClass(/open/);

  // The coupon really was spent: this tab now holds a credential it had no
  // other way of learning. What it holds changed in SH-254 — it was the
  // daemon's master token, and it is a scoped session capability now, so the
  // assertion is written in both directions. A tab holding the token could
  // delete every project on the machine and reach `POST /api/v1/invoke`; this
  // one can edit stories and states, on this machine only, revocably.
  await expect
    .poll(() =>
      page.evaluate(() =>
        window.sessionStorage.getItem("storyhookDashboardSession"),
      ),
    )
    .toMatch(/^[0-9a-f]{32}$/);
  expect(
    await page.evaluate(() =>
      window.sessionStorage.getItem("storyhookDaemonToken"),
    ),
  ).not.toBe(requiredEnv("DASHBOARD_TOKEN"));

  // And every request this page has made so far was constructed with the
  // fragment already gone. The length check is the harness guard: a shim that
  // captured nothing would otherwise satisfy `every()` vacuously.
  const hashes = await hashesAtRequest(page);
  expect(hashes.length).toBeGreaterThan(0);
  expect(hashes).toEqual(hashes.map(() => ""));
});

test("that same tab writes without ever being asked for a token", async ({
  page,
  request,
}) => {
  // The half `loopback-no-token.spec.ts` cannot have: there, the first write
  // prompts, because a hand-typed URL carries no credential. Here it must
  // not — which is the whole point of the story, and the thing a read-only
  // assertion above could never distinguish from SH-250 alone.
  const coupon = await armCoupon(request);
  await page.goto(`/#h=${coupon}`);

  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await expect(page.locator("#board-view")).toBeVisible();

  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page
    .locator("#create-title")
    .fill("SH-251 — created by a handed-off tab");
  await page.locator("#create-submit").click();

  await expect(
    page.locator(".card", { hasText: "SH-251 — created by a handed-off tab" }),
  ).toBeVisible();
  await expect(page.locator("#token-modal")).not.toHaveClass(/open/);
});

test("that same tab is shown the boundary rather than a button that 403s", async ({
  page,
  request,
}) => {
  // SH-254's affordance half. The server refuses dispatch to a session
  // capability whatever this page renders — `tests/session_capability.rs` is
  // where that is proved — but a user who clicks and gets nothing has learned
  // nothing. So the control is disabled and carries the command that works.
  const coupon = await armCoupon(request);
  await page.goto(`/#h=${coupon}`);

  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await expect(page.locator("#board-view")).toBeVisible();
  await page.locator(".card").first().click();

  const dispatch = page.locator("#dispatch-btn");
  await expect(dispatch).toBeDisabled();
  await expect(dispatch).toHaveAttribute("title", /story dispatch/);

  // And the modal stays shut: meeting the edge of a scope is not a reason to
  // ask anybody for the strongest credential on the machine.
  await expect(page.locator("#token-modal")).not.toHaveClass(/open/);
});

test("a coupon somebody already spent renders the dashboard and says so once", async ({
  page,
  request,
}) => {
  // Redeemed out from under the browser before it ever loads — the shape a
  // stale link, a reused shell-history entry, or an actual theft takes.
  const coupon = await armCoupon(request);
  const spent = await request.post("/handoff", {
    headers: { "X-Storyhook": "1", "X-Storyhook-Handoff": coupon },
  });
  expect(spent.ok()).toBe(true);

  await page.goto(`/#h=${coupon}`);

  // Render anyway, on SH-250's tokenless read: a refused handoff must not
  // leave a blank page.
  await expect(
    page.locator(".repo-card-name", { hasText: "Alpha Project" }),
  ).toBeVisible();

  // Said once, informationally. Never the modal: opening it pre-emptively is
  // exactly the behaviour SH-250's client change removed, and a refused
  // coupon is not a reason to reintroduce it.
  await expect(page.locator(".toast")).toHaveCount(1);
  await expect(page.locator(".toast")).toContainText("not accepted");
  await expect(page.locator("#token-modal")).not.toHaveClass(/open/);

  // Nothing was stored, so the tab is exactly a hand-typed one: it reads, and
  // it will prompt when it writes.
  expect(
    await page.evaluate(() =>
      window.sessionStorage.getItem("storyhookDaemonToken"),
    ),
  ).toBeNull();
});
