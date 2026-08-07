import { test, expect } from "@playwright/test";

/**
 * SH-145: a dashboard tab must not stay silently stale forever.
 *
 * `EventSource` only reports an error when its underlying connection
 * actually closes, and a connection that goes silently dead — a laptop
 * sleeping, a NAT mapping expiring mid-idle — may never close at all: a
 * browser's TCP stack can keep accepting the daemon's small, infrequent
 * heartbeat writes without them ever crossing a link that no longer exists.
 * Nothing in `EventSource`'s own machinery ever reconnects a tab like that.
 * `sseWatchdog()` (`web_dashboard.html`) is the fix: it tracks the last time
 * any SSE message — a push, or the daemon's `ping` heartbeat — arrived, and
 * force-reconnects once that gap exceeds a threshold.
 *
 * This is exercised for real, not mocked: the query-string overrides below
 * shrink the watchdog's own timing (mirroring `STORYHOOK_SSE_HEARTBEAT_MS` on
 * the daemon side) to something this suite's test budget can outwait, but
 * the seeded daemon's real ~20s heartbeat interval is untouched. Inside this
 * test's short observation window it simply never fires — no fault
 * injection is needed to produce the silence the watchdog is supposed to
 * notice; the default heartbeat cadence already is one.
 */

test("a stale connection is replaced without a page reload", async ({
  page,
}) => {
  const repoListRequests: number[] = [];
  page.on("request", (req) => {
    const url = new URL(req.url());
    if (req.method() === "GET" && url.pathname === "/api/repos") {
      repoListRequests.push(Date.now());
    }
  });

  const staleAfterMs = 400;
  const watchdogIntervalMs = 100;
  await page.goto(
    `/?sseStaleAfterMs=${staleAfterMs}&sseWatchdogIntervalMs=${watchdogIntervalMs}`,
  );
  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await expect(page.locator("#board-view")).toBeVisible();
  await expect(page.locator("#conn-text")).toHaveText("Live");

  // Bootstrap's own fetches (the initial page load, `onopen` of the first
  // connection, opening Alpha's board) are done well within this margin —
  // everything counted from here on is attributable to what happens next.
  await page.waitForTimeout(200);
  const requestsBeforeStale = repoListRequests.length;

  // Comfortably past the watchdog noticing the silence and reconnecting.
  await page.waitForTimeout(staleAfterMs + watchdogIntervalMs * 2 + 300);

  expect(
    repoListRequests.length,
    "the watchdog's reconnect should have triggered a fresh GET /api/repos " +
      "(via the new connection's `onopen` resync) with no story change, no " +
      "user action, and the 25s safety poll nowhere near due",
  ).toBeGreaterThan(requestsBeforeStale);

  // The reconnect went through `EventSource`'s own machinery, not a reload:
  // the board is still showing the project it was on.
  await expect(page.locator("#board-view")).toBeVisible();
  await expect(page.locator("#projsel-btn")).toContainText(
    "AA · Alpha Project",
  );
});
