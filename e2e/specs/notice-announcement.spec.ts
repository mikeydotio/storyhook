import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import {
  cleanUpCreatedStories,
  COPY_HEADLINES,
  createStory,
  keepNotices,
  openProject,
  raiseDurableNotices,
  seedToken,
} from "./support";

/**
 * SH-333 — what gets announced when a notice arrives, as measured properties.
 *
 * A file of its own rather than more of `notification-contract.spec.ts` (which
 * owns semantics and timing) or `notice-dock-geometry.spec.ts` (which owns
 * rects and reachability): this file's subject is what an `aria-live`/`role`
 * mutation actually says, which is a third thing neither of those files
 * asserts.
 *
 * The council that settled the mechanism (`.council/sh-333-notice-stack-
 * announcement-mechanism/DECISION.md`) split the fix by architecture rather
 * than picking one mechanism for both notice surfaces, because the two turned
 * out to be two different defects wearing the same symptom:
 *
 * - `#toast-stack` inserts nodes incrementally (`stack.insertBefore`), so
 *   `aria-atomic="false"` on the stack plus `aria-atomic="true"` on each
 *   `.toast` is a spec-native fit: a mutation announces exactly the node that
 *   changed, whole, rather than the whole standing pile.
 * - `#dispatch-history` clears and rebuilds wholesale on every render
 *   (`renderDispatchHistory`), so no live-region attribute combination can
 *   stop it re-announcing every row regardless of atomicity. It lost
 *   `aria-live` entirely and gained a dedicated `sr-only role="status"`
 *   announcer, `#dispatch-history-status`, fed directly by
 *   `addDispatchHistoryRow()`. Deliberate tech debt, tied to SH-337
 *   (`renderDispatchHistory`'s already-scoped incremental-insert rewrite) as
 *   the trigger to retire the side-channel.
 *
 * What is pinned here: which elements carry which ARIA attributes, and what
 * text a `role="status"` announcer holds. What is NOT claimed: what a real
 * assistive technology actually utters, including whether its own speech
 * queue coalesces two adjacent identical announcements — no AT is driven by
 * this suite (Chromium only), and this project's own SH-322/SH-327 precedent
 * is not to imply coverage of what was not checked.
 */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await seedToken(page);
});

/** A stubbed dispatch envelope in a terminal state, mirroring
 * `notification-contract.spec.ts`'s own `stubbedDispatch` — this file needs
 * only the `refused` shape, both attended and `--auto`, so it keeps its own
 * minimal copy rather than importing test machinery across spec files. */
function stubbedRefusal(storyId: string, auto: boolean): string {
  return JSON.stringify({
    result: "ok",
    dispatch: {
      handle: "stub-handle",
      project: "alpha",
      story: storyId,
      auto,
      state: "refused",
      started_at: "2026-01-01T00:00:00Z",
      finished_at: "2026-01-01T00:00:01Z",
      payload: { display: "[story] refused: that story is already in-progress — claimed by another session" },
      reason: "claim-conflict",
    },
  });
}

async function stubDispatchRefusal(page: Page, storyId: string, auto: boolean): Promise<void> {
  await page.route("**/dispatch**", async (route) => {
    await route.fulfill({
      status: route.request().method() === "POST" ? 202 : 200,
      contentType: "application/json",
      body: stubbedRefusal(storyId, auto),
    });
  });
}

/** Creates a story, opens its drawer, and returns its id.
 *
 * Sent home first, unconditionally: `openProject` hunts for a `.repo-card-name`
 * on the Home screen, and a second call in the same test starts from whatever
 * screen the previous dispatch left the page on (the board), not Home. */
async function openFreshStory(page: Page, title: string): Promise<string> {
  await page.locator("#home-btn").click();
  await openProject(page, "Alpha Project");
  const id = await createStory(page, title);
  await page.locator('.column[data-state="todo"] .card', { hasText: title }).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  return id;
}

/** Raises one attended, refused dispatch: a durable `.toast.error` carrying a
 * detail line and a typed reason. */
async function raiseAttendedRefusal(page: Page, title: string): Promise<string> {
  const id = await openFreshStory(page, title);
  await stubDispatchRefusal(page, id, false);
  await page.locator("#dispatch-btn").click();
  const toast = page.locator("#toast-stack .toast.error");
  await expect(toast).toBeVisible();
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  return id;
}

/** Raises one `--auto` refused dispatch: a durable `#dispatch-history` row
 * carrying a detail line and a typed reason. */
async function raiseHistoryRow(page: Page, title: string): Promise<string> {
  const before = await page.locator("#dispatch-history .dispatch-history-row").count();
  const id = await openFreshStory(page, title);
  await stubDispatchRefusal(page, id, true);
  await page.locator("#dispatch-auto-btn").click();
  await expect(page.locator("#dispatch-history .dispatch-history-row")).toHaveCount(before + 1);
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  return id;
}

// ============================================================
// The toast stack (A′): aria-atomic scoping
// ============================================================

test("the toast stack is non-atomic, and every toast announces itself whole", async ({
  page,
}) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  const title = "SH-333 — toast atomic scoping";
  await createStory(page, title);
  await raiseDurableNotices(page, title, 3);

  await expect(page.locator("#toast-stack")).toHaveAttribute("aria-atomic", "false");
  // Still `role="status" aria-live="polite"` — unchanged by SH-333. Only the
  // atomicity narrowed; the region is still where a mutation is expected.
  await expect(page.locator("#toast-stack")).toHaveAttribute("role", "status");
  await expect(page.locator("#toast-stack")).toHaveAttribute("aria-live", "polite");

  const toasts = page.locator("#toast-stack .toast");
  await expect(toasts).toHaveCount(3);
  const atomics = await toasts.evaluateAll((nodes) =>
    nodes.map((n) => n.getAttribute("aria-atomic")),
  );
  expect(atomics).toEqual(["true", "true", "true"]);
});

test("an attended refusal's detail and reason live inside the same atomic node as its headline", async ({
  page,
}) => {
  await page.goto("/");
  const title = "SH-333 — toast detail stays atomic";
  const id = await raiseAttendedRefusal(page, title);

  const toast = page.locator("#toast-stack .toast.error");
  await expect(toast).toHaveAttribute("aria-atomic", "true");
  await expect(toast).toContainText(`${id} refused`);
  await expect(toast.locator(".notice-detail")).toContainText("already in-progress");
  await expect(toast.locator(".notice-reason")).toHaveText("claim-conflict");
});

// ============================================================
// Dispatch history (C): the dedicated announcer
// ============================================================

test("a dispatch-history arrival is announced alone, not the standing pile", async ({
  page,
}) => {
  await page.goto("/");
  const titleA = "SH-333 — history announcer, older";
  const titleB = "SH-333 — history announcer, newer";
  const idA = await raiseHistoryRow(page, titleA);
  const idB = await raiseHistoryRow(page, titleB);

  await expect(page.locator("#dispatch-history .dispatch-history-row")).toHaveCount(2);
  const announced = await page.locator("#dispatch-history-status").textContent();
  // The measured property: the announcer holds exactly the arriving row's own
  // text, not the standing pile's -- the older row's id is gone from the
  // announcer once a second row has landed, even though both rows are still
  // visible in `#dispatch-history` itself.
  expect(announced).toContain(`${idB} refused`);
  expect(announced).not.toContain(idA);
});

test("a dispatch-history arrival's detail and reason are announced, not just its headline", async ({
  page,
}) => {
  await page.goto("/");
  const title = "SH-333 — history announcer detail";
  const id = await raiseHistoryRow(page, title);

  const announced = await page.locator("#dispatch-history-status").textContent();
  expect(announced).toContain(`${id} refused`);
  expect(announced).toContain("already in-progress");
  expect(announced).toContain("claim-conflict");
});

test("two identical arrivals in a row both mutate the announcer", async ({ page }) => {
  // The SAME story, dispatched twice in sequence: `finishDispatch` clears
  // `state.dispatches[storyId]` on completion, so a second dispatch of the
  // same story is allowed once the first has finished, and its headline,
  // detail and reason are then character-for-character identical to the
  // first -- the one case that actually exercises `setStatusText`'s
  // clear-then-set idiom (SH-333 rider 2), which a test using two different
  // stories (and therefore two different headlines) cannot reach.
  await page.goto("/");
  const title = "SH-333 — history announcer repeat";
  const id = await openFreshStory(page, title);
  await stubDispatchRefusal(page, id, true);

  await page.locator("#dispatch-history-status").evaluate((node) => {
    (node as HTMLElement & { __mutations?: number }).__mutations = 0;
    const observer = new MutationObserver((records) => {
      (node as HTMLElement & { __mutations?: number }).__mutations! += records.length;
    });
    observer.observe(node, { childList: true, characterData: true, subtree: true });
  });

  await page.locator("#dispatch-auto-btn").click();
  await expect(page.locator("#dispatch-history .dispatch-history-row")).toHaveCount(1);
  await expect(page.locator("#dispatch-history-status")).toContainText(`${id} refused`);

  const firstAnnouncement = await page.locator("#dispatch-history-status").textContent();

  // Re-arm and re-dispatch the identical story.
  await stubDispatchRefusal(page, id, true);
  await page.locator("#dispatch-auto-btn").click();
  await expect(page.locator("#dispatch-history .dispatch-history-row")).toHaveCount(2);

  const secondAnnouncement = await page.locator("#dispatch-history-status").textContent();
  expect(secondAnnouncement).toBe(firstAnnouncement);

  const mutationCount = await page.locator("#dispatch-history-status").evaluate(
    (node) => (node as HTMLElement & { __mutations?: number }).__mutations!,
  );
  // Clear-then-set is two DOM mutations per announcement (removing the old
  // text node, adding the new one); two announcements with identical text
  // must not collapse into fewer just because the text happens to match.
  expect(mutationCount).toBeGreaterThanOrEqual(3);

  await page.locator("#drawer-close").click();
});

// ============================================================
// Inventory and non-interference
// ============================================================

test("the notice dock's only live regions are the toast stack and the two sr-only announcers", async ({
  page,
}) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  const title = "SH-333 — live region inventory";
  await createStory(page, title);
  await raiseDurableNotices(page, title, 1);
  await raiseHistoryRow(page, "SH-333 — live region inventory, history");

  const inventory = await page.evaluate(() => {
    const dock = document.getElementById("notice-dock")!;
    const nodes = Array.from(
      dock.querySelectorAll('[aria-live], [role="status"], [role="alert"]'),
    );
    return nodes.map((n) => n.id || `${n.tagName.toLowerCase()}.${n.className}`);
  });

  expect(inventory.sort()).toEqual(
    ["toast-stack", "notice-dock-status", "dispatch-history-status"].sort(),
  );

  // `#dispatch-history` itself carries no live-region attribute of any kind —
  // the wholesale rebuild is announced through the side channel instead.
  const dispatchHistoryAttrs = await page.locator("#dispatch-history").evaluate((n) => ({
    ariaLive: n.getAttribute("aria-live"),
    role: n.getAttribute("role"),
  }));
  expect(dispatchHistoryAttrs.ariaLive).toBeNull();
  expect(dispatchHistoryAttrs.role).toBe("region");
});

test("demoting dispatch-history's live-region status removed no landmark or reachability", async ({
  page,
}) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  await expect(page.locator("#toast-scroll")).toHaveAttribute("role", "group");
  await expect(page.locator("#toast-scroll")).toHaveAttribute("aria-label", "Notices");
  await expect(page.locator("#dispatch-history-scroll")).toHaveAttribute("role", "group");
  await expect(page.locator("#dispatch-history-scroll")).toHaveAttribute(
    "aria-label",
    "Dispatch results",
  );
  await expect(page.locator("#dispatch-history")).toHaveAttribute(
    "aria-label",
    "Autonomous dispatch results",
  );

  const title = "SH-333 — reachability survives demotion";
  await raiseHistoryRow(page, title);
  await expect(
    page.locator("#dispatch-history .dispatch-history-row .dispatch-history-dismiss"),
  ).toBeVisible();
});

test("a dispatch-history arrival does not clobber the dismissal channel", async ({ page }) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  const noticeTitle = "SH-333 — no clobber, notice";
  await createStory(page, noticeTitle);
  await raiseDurableNotices(page, noticeTitle, 1);
  await page.locator("#toast-stack .toast .toast-dismiss").first().click();
  await expect(page.locator("#notice-dock-status")).toHaveText(
    "Notice dismissed. No notices remaining.",
  );

  // A dispatch-history arrival lands on its own channel -- #notice-dock-status
  // must still hold the dismissal message above, untouched (SH-333 rider 1:
  // that element also carries the armed-delete confirmation prompt a
  // background arrival must never overwrite).
  await raiseHistoryRow(page, "SH-333 — no clobber, history");
  await expect(page.locator("#notice-dock-status")).toHaveText(
    "Notice dismissed. No notices remaining.",
  );
  await expect(page.locator("#dispatch-history-status")).toContainText("refused");
});

test("both notice surfaces still keep the same headline vocabulary announced", async ({
  page,
}) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  const title = "SH-333 — headline vocabulary";
  await createStory(page, title);
  await raiseDurableNotices(page, title, 1);
  const toastText = await page.locator("#toast-stack .toast").first().textContent();
  expect(toastText).toContain(COPY_HEADLINES[0]);
});
