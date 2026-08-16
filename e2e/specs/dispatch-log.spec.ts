import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import { requiredEnv, seedToken } from "./support";

/**
 * SH-361 — the Settings screen's Dispatch log, the reader the daemon's
 * finished `DispatchRecord`s never had.
 *
 * A durable **error** notice used to be the only record of the failure it
 * reported. `addDispatchHistoryRow`'s own doc comment said so, and dismissing
 * the notice — deliberately, or through the "Dismiss all" bar SH-323 added —
 * destroyed it. SH-339 fixed the *amplification* (a held Enter walking the
 * heir chain); this story fixes the *loss*, for the half of it that has a
 * record to recover.
 *
 * ## What this file claims, and what it deliberately does not
 *
 * The story's requirement is that "a reader can get the failed dispatch's
 * detail and typed reason back after the notice is gone." **That claim is
 * layered across two suites on purpose, and no single test joins the layers
 * in one process** — SH-361's council ruled it so, and this comment is where
 * it said to record that.
 *
 *   - The DAEMON half is Rust's: `src/api/dispatch.rs`'s own tests prove a
 *     finished record keeps its `payload.display` and its typed `reason`,
 *     that the route serves them, that the list survives a daemon restart,
 *     and that reading the log evicts nothing.
 *   - The CLIENT half is this file's: that the page reads the ROUTE rather
 *     than its own memory, renders both lines through the dock's own
 *     `noticeBody`, and discloses the bound rather than reading as complete.
 *
 * The seam is not laziness. The only refusal a browser suite can provoke on
 * demand in the shared e2e daemon is `fail()`-shaped and carries **no**
 * `reason` at all, so a "real" end-to-end reason assertion would pass
 * vacuously; the deterministic alternatives (`STORYHOOK_DISPATCH_SCRIPT`, a
 * charter-inert `STORY_PROMPT_EXTRA`) are daemon-wide and would poison
 * `dispatch.spec.ts`'s real dispatches, which run against the same daemon.
 * This is the SH-322/SH-327 precedent: say what is checked and what is not,
 * rather than implying coverage nobody has.
 *
 * One thing IS asserted unstubbed, because it can be: the route really
 * exists on the real daemon, and the numbers the page renders are the
 * numbers that daemon reports.
 */

const DASHBOARD_TOKEN = requiredEnv("DASHBOARD_TOKEN");

/** A stubbed `GET /api/dispatch-log` envelope — the shape
 * `DispatchLogEnvelope` serializes in `src/api/dispatch.rs`. */
function stubbedLog(
  dispatches: Array<Record<string, unknown>>,
  retention: { records: number; seconds: number; forgotten: number },
) {
  return JSON.stringify({ result: "ok", dispatches, retention });
}

/** One refused record carrying both diagnosis lines a dismissed notice showed. */
const REFUSED = {
  handle: "stub-1",
  project: "alpha",
  story: "AA-7",
  auto: false,
  state: "refused",
  started_at: "2026-01-01T00:00:00Z",
  finished_at: "2026-01-01T00:00:01Z",
  payload: { display: "[story] refused: that story is already in-progress" },
  reason: "claim-conflict",
};

/** A failed record with NO typed reason — `DispatchReason` is absent for a
 * `fail()`-shaped refusal and for every `failed` record, and `dispatch.rs`
 * says that absence is meaningful rather than a gap. */
const FAILED_NO_REASON = {
  handle: "stub-2",
  project: "alpha",
  story: "AA-8",
  auto: true,
  state: "failed",
  started_at: "2026-01-01T00:00:00Z",
  finished_at: "2026-01-01T00:00:02Z",
  error: "the dispatch script exited without printing a result",
};

const RETENTION = { records: 32, seconds: 1800, forgotten: 0 };

async function stubLog(
  page: Page,
  dispatches: Array<Record<string, unknown>>,
  retention = RETENTION,
): Promise<void> {
  await page.route("**/api/dispatch-log", async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: stubbedLog(dispatches, retention),
    });
  });
}

async function openSettings(page: Page): Promise<void> {
  await page.locator("#settings-btn").click();
  await expect(page.locator("#settings-view")).toBeVisible();
  await expect(page.locator("#dispatch-log")).toBeVisible();
}

test.beforeEach(async ({ page }) => {
  await seedToken(page);
});

test("the route is real, and the page's sentence is the daemon's own numbers", async ({
  page,
  request,
}) => {
  // Unstubbed, both sides: the route really is served by the real daemon,
  // and the page's sentence agrees with what that daemon reports.
  //
  // **This test does NOT catch the hand-copy, and an earlier version of this
  // comment claimed it did.** Measured: hard-coding `30` and `32` in
  // `dispatchLogPolicySentence` leaves this test GREEN, because the real
  // daemon's numbers are 30 minutes and 32 records, so a literal
  // coincidentally matches the thing it is supposed to be derived from. The
  // pin that actually kills that mutation is the disclosure test below,
  // which rewrites `retention` to 7 and 120 -- values no literal can match.
  // Recorded rather than quietly fixed, because "assert against production's
  // own value" is a shape that looks like a derivation pin and is not one
  // whenever production's value is the plausible literal.
  const resp = await request.get("/api/dispatch-log", {
    headers: { "X-Storyhook-Token": DASHBOARD_TOKEN },
  });
  expect(resp.status()).toBe(200);
  const body = await resp.json();
  expect(body.result).toBe("ok");
  expect(Array.isArray(body.dispatches)).toBe(true);
  expect(typeof body.retention.records).toBe("number");
  expect(typeof body.retention.seconds).toBe("number");

  await page.goto("/");
  await openSettings(page);
  const policy = page.locator("#dispatch-log-policy");
  await expect(policy).toBeVisible();
  await expect(policy).toContainText(`${Math.round(body.retention.seconds / 60)} minute`);
  await expect(policy).toContainText(`${body.retention.records} newer dispatches`);
});

test("a record is recovered in a browser context that never saw its notice", async ({
  browser,
}) => {
  // THE pin the story asked for, in the form that falsifies the most.
  //
  // A fresh context has an empty cookie jar, empty sessionStorage, empty
  // localStorage and no in-memory `state.dispatchHistory` -- so an
  // implementation that recorded the notice CLIENT-side, by any of those
  // three routes, renders nothing here. `waitForRequest` is asserted
  // alongside, so a page that somehow rendered from client state without
  // ever calling the route fails rather than passing on coincidence.
  const context = await browser.newContext();
  const page = await context.newPage();
  await seedToken(page);
  await stubLog(page, [REFUSED]);

  const logRequest = page.waitForRequest("**/api/dispatch-log");
  await page.goto("/");
  await openSettings(page);
  await logRequest;

  const row = page.locator(".dispatch-log-row", { hasText: "AA-7" });
  await expect(row).toBeVisible();
  // Composed through `dispatchHeadline` and `noticeBody` -- the dock's own
  // renderer, reused rather than reimplemented, which is what makes this
  // text the dismissed notice's text rather than an approximation of it.
  await expect(row.locator(".notice-headline")).toHaveText("AA-7 refused");
  await expect(row.locator(".notice-detail")).toContainText("already in-progress");
  await expect(row.locator(".notice-reason")).toHaveText("claim-conflict");

  await context.close();
});

test("a record with no typed reason renders none, and is still not a blank row", async ({
  page,
}) => {
  await stubLog(page, [FAILED_NO_REASON]);
  await page.goto("/");
  await openSettings(page);

  const row = page.locator(".dispatch-log-row", { hasText: "AA-8" });
  await expect(row).toBeVisible();
  // The absence is honest: `dispatch.rs` says a missing `reason` is
  // meaningful, so an empty `.notice-reason` would make a real absence look
  // like a rendering bug.
  await expect(row.locator(".notice-reason")).toHaveCount(0);
  // And the row still says something -- `error` carries the detail, so
  // "no reason" never means "no information".
  await expect(row.locator(".notice-detail")).toContainText("without printing a result");
});

test("the bound is disclosed, as a floor, and states the rule even when empty", async ({
  page,
}) => {
  // SH-306's "no silent caps" on the exact path where it would ship
  // silently. `forgotten` is rendered as "at least N" and never as a count:
  // it resets on a daemon restart and omits an expired-but-uncollected
  // record, so it under-reports for two independent reasons, and an
  // exact-sounding number would be a lie.
  await stubLog(page, [], { records: 7, seconds: 120, forgotten: 3 });
  await page.goto("/");
  await openSettings(page);

  const policy = page.locator("#dispatch-log-policy");
  // Composed from the response, so rewritten numbers appear verbatim.
  await expect(policy).toContainText("2 minutes");
  await expect(policy).toContainText("7 newer dispatches");
  await expect(policy).toContainText("At least 3 older results have already been discarded");
  // Stated even with nothing in the list -- empty is exactly where a reader
  // needs to know whether nothing ran or their result aged out.
  await expect(page.locator(".dispatch-log-list")).toContainText(
    "No dispatch results are being kept right now",
  );
});

test("nothing dropped is not reported as something dropped", async ({ page }) => {
  await stubLog(page, [REFUSED], { records: 32, seconds: 1800, forgotten: 0 });
  await page.goto("/");
  await openSettings(page);
  const policy = page.locator("#dispatch-log-policy");
  await expect(policy).toBeVisible();
  await expect(policy).not.toContainText("At least");
});

test("the log is not a live region", async ({ page }) => {
  // A copy-paste from the dock's markup would announce the whole list on
  // every Settings visit, over whatever the dock is already saying --
  // SH-333's defect on a new surface. The page's live regions stay exactly
  // the three it has had since SH-337, plus this section's own sr-only
  // announcer, which is OUTSIDE the list and speaks only for a Refresh.
  await stubLog(page, [REFUSED, FAILED_NO_REASON]);
  await page.goto("/");
  await openSettings(page);

  const live = await page.evaluate(() =>
    Array.from(document.querySelectorAll("#dispatch-log [aria-live], #dispatch-log [role]"))
      .map((node) => `${node.id || node.className}:${node.getAttribute("role") || ""}:${node.getAttribute("aria-live") || ""}`),
  );
  expect(live).toEqual(["dispatch-log-status:status:"]);
});

test("a failed fetch reports inline and does not raise the notice it would then lose", async ({
  page,
}) => {
  // The recursion this story exists to prevent: a durable error notice ABOUT
  // the recovery surface, which the reader then dismisses, is the filed
  // defect eating its own fix.
  await page.route("**/api/dispatch-log", async (route) => route.abort());
  await page.goto("/");
  await openSettings(page);

  // The sentence comes from `readinessNote`, the page's one readiness
  // generator (SH-291/SH-301, fenced by `web_test.rs`), so it also carries
  // that generator's "Retrying…" promise -- which this section keeps with a
  // failure-only retry rather than claiming and not doing.
  await expect(page.locator("#dispatch-log-body .modal-error")).toContainText(
    "Couldn't load the dispatch log. Retrying…",
  );
  await expect(page.locator("#toast-stack .toast")).toHaveCount(0);
});

test("a dispatch stub does not swallow the log route", async ({ page }) => {
  // Two council seats found this hazard independently, before the route
  // existed: `page.route("**/dispatch**")` -- the glob nine call sites used
  // -- also matches `/api/dispatch-log`, and would answer it with a
  // `DispatchEnvelope` (`{result, dispatch}`) that this reader cannot parse.
  // `tests/dispatch_route_stubs.rs` fences the class statically; this is the
  // behavioural half, registering a dispatch stub in the narrowed shape and
  // proving the log still resolves beside it.
  await page.route("**/story/*/dispatch**", async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ result: "ok", dispatch: { handle: "x", state: "ok" } }),
    });
  });
  await stubLog(page, [REFUSED]);
  await page.goto("/");
  await openSettings(page);
  await expect(page.locator(".dispatch-log-row", { hasText: "AA-7" })).toBeVisible();
});
