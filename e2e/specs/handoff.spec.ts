import { test, expect } from "./support";
import type { APIRequestContext, Page } from "@playwright/test";
import { cleanUpCreatedStories, openProject, requiredEnv } from "./support";

/**
 * SH-251's one-click dashboard, in a real browser: a tab opened at
 * `#h=<coupon>` spends it for a named token before it issues a single
 * request, and never prompts for anything.
 *
 * What a coupon buys changed twice since these specs were first written.
 * Until SH-254 it bought the daemon's master token; SH-254 then substituted
 * a scoped session capability, readable from `sessionStorage` and refused
 * dispatch and project CRUD outright. SH-255 deletes that tier along with
 * every other credential concept but one: a coupon now buys a full named
 * token, delivered as an `HttpOnly` cookie this page can never read back --
 * same as one minted by `story token new` or pasted into the modal, just
 * issued by a click instead. There is nothing left for a handed-off tab to
 * be scoped away from.
 *
 * **These specs deliberately do NOT call `seedToken`.** Like
 * `loopback-requires-a-token.spec.ts`, the whole claim is about a browser
 * that starts with nothing — a seeded credential would make every assertion
 * here pass for the wrong reason.
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

test("a tab opened with a coupon holds a named token as an invisible cookie", async ({
  page,
  request,
}) => {
  const coupon = await armCoupon(request);
  await recordHashAtEachRequest(page);

  await page.goto(`/#h=${coupon}`);

  // The dashboard renders, with nothing typed and nothing seeded -- proving
  // the redeemed credential really does authenticate GET /api/repos and
  // GET .../data, the two reads SH-255 stopped exempting.
  await expect(
    page.locator(".repo-card-name", { hasText: "Alpha Project" }),
  ).toBeVisible();
  await expect(page.locator("#token-modal")).not.toHaveClass(/open/);

  // The coupon really was spent, for a credential this page can never read
  // back. `Set-Cookie ... HttpOnly` means `document.cookie` never contains
  // it -- unlike SH-254's session (readable, held in `sessionStorage`) or
  // SH-251's original master-token handoff, nothing this page's own
  // JavaScript can inspect ever holds the secret. `that same tab writes
  // without ever being asked for a token`, below, is the positive proof that
  // something real is there regardless.
  const cookieName = requiredEnv("DASHBOARD_COOKIE_NAME");
  const visibleCookie = await page.evaluate(() => document.cookie);
  expect(visibleCookie).not.toContain(cookieName);

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
  // The half `loopback-requires-a-token.spec.ts` cannot have: there, the
  // first write prompts, because a hand-typed URL carries no credential.
  // Here it must not -- which is the whole point of the story, and the thing
  // a read-only assertion above could never distinguish from a stray
  // exemption.
  const coupon = await armCoupon(request);
  await page.goto(`/#h=${coupon}`);

  await openProject(page, "Alpha Project");

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

test("that same tab's Dispatch button is fully enabled, not held to a narrower scope", async ({
  page,
  request,
}) => {
  // SH-254's affordance is gone along with the scoped session it existed
  // for. A coupon now buys a full named token (SH-255) -- there is no
  // narrower tier left for a control to be held to, so the button that used
  // to render disabled here renders exactly as it does for any other
  // authenticated tab. Checked without clicking it: Alpha's own stories are
  // each claimed by `dispatch.spec.ts`'s real-dispatch tests, and this file
  // only needs to know the button is live, not run a second real dispatch.
  const coupon = await armCoupon(request);
  await page.goto(`/#h=${coupon}`);

  await openProject(page, "Alpha Project");
  await page.locator(".card").first().click();

  await expect(page.locator("#dispatch-btn")).toBeEnabled();
  await expect(page.locator("#token-modal")).not.toHaveClass(/open/);
});

test("a coupon somebody already spent shows the refusal, then prompts like any other tab", async ({
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

  // Said once, informationally -- `redeemHandoff`'s own refusal path, which
  // never retries a coupon that might have just been stolen.
  await expect(page.locator(".toast")).toHaveCount(1);
  await expect(page.locator(".toast")).toContainText("not accepted");

  // And then the token modal, same as any tab with no credential at all:
  // SH-255 deleted the tokenless read exemption a refused coupon used to
  // fall back on, so `fetchReposOnce`'s own 401 handler is what takes over
  // from here, exactly as it would for a hand-typed URL.
  await expect(page.locator("#token-modal")).toHaveClass(/open/);
  await expect(
    page.locator(".repo-card-name", { hasText: "Alpha Project" }),
  ).toHaveCount(0);
});
