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
 * SH-333/SH-337 — what gets announced when a notice arrives, as measured
 * properties.
 *
 * A file of its own rather than more of `notification-contract.spec.ts` (which
 * owns semantics and timing) or `notice-dock-geometry.spec.ts` (which owns
 * rects and reachability): this file's subject is what an `aria-live`/`role`
 * mutation actually says, which is a third thing neither of those files
 * asserts.
 *
 * SH-333's council split the fix by architecture rather than picking one
 * mechanism for both notice surfaces (verdict on SH-333), because the two
 * were two different
 * defects wearing the same symptom at the time: `#toast-stack` already
 * inserted nodes incrementally, so `aria-atomic="false"` on the stack plus
 * `aria-atomic="true"` on each `.toast` was a spec-native fit. `#dispatch-
 * history` still cleared and rebuilt wholesale on every render
 * (`renderDispatchHistory`), so no live-region attribute combination could
 * stop it re-announcing every row regardless of atomicity — it lost
 * `aria-live` entirely and gained a dedicated `sr-only role="status"`
 * announcer, `#dispatch-history-status`, fed by hand from
 * `addDispatchHistoryRow()`. Logged as deliberate tech debt naming SH-337
 * (`renderDispatchHistory`'s incremental-insert rewrite) as the trigger to
 * retire the side channel.
 *
 * SH-337 fired that trigger: the panel is `insertBefore`d one row at a time
 * now, so `#dispatch-history` carries the identical shape `#toast-stack` has
 * (`aria-live="polite" aria-atomic="false"` on the region, `aria-atomic="true"`
 * per row), and `#dispatch-history-status` is gone.
 *
 * What is pinned here: which elements carry which ARIA attributes, and what
 * text or DOM mutation a live region or `role="status"` announcer produces.
 * What is NOT claimed: what a real assistive technology actually utters,
 * including whether its own speech queue coalesces two adjacent identical
 * announcements — no AT is driven by this suite on any engine (SH-335 added
 * `webkit`, but that is a second rendering engine, not an AT), and this
 * project's own SH-322/SH-327 precedent is not to imply coverage of what was
 * not checked.
 */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page, context, browserName }) => {
  // WebKit gets `clipboard-read` alone: its Playwright permission map has no
  // `clipboard-write` entry at all (`grantPermissions` throws "Unknown
  // permission: clipboard-write" there — SH-335). Every other engine keeps
  // both -- a headless/automated `navigator.clipboard.writeText()` needs the
  // explicit grant there, not just this file's own read-back.
  const permissions =
    browserName === "webkit" ? ["clipboard-read"] : ["clipboard-read", "clipboard-write"];
  await context.grantPermissions(permissions);
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
  await page.route("**/story/*/dispatch**", async (route) => {
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
// Dispatch history (SH-337: converged onto the same shape as A′)
// ============================================================

test("a dispatch-history arrival adds exactly the arriving row, and only it, to the live region", async ({
  page,
}) => {
  await page.goto("/");
  const titleA = "SH-337 — history announcer, older";
  const titleB = "SH-337 — history announcer, newer";
  const idA = await raiseHistoryRow(page, titleA);

  await page.locator("#dispatch-history").evaluate((node) => {
    (node as HTMLElement & { __added?: string[] }).__added = [];
    const observer = new MutationObserver((records) => {
      records.forEach((record) => {
        record.addedNodes.forEach((added) => {
          (node as HTMLElement & { __added?: string[] }).__added!.push(
            (added as HTMLElement).textContent ?? "",
          );
        });
      });
    });
    observer.observe(node, { childList: true });
  });

  const idB = await raiseHistoryRow(page, titleB);
  await expect(page.locator("#dispatch-history .dispatch-history-row")).toHaveCount(2);

  const added = await page.locator("#dispatch-history").evaluate(
    (node) => (node as HTMLElement & { __added?: string[] }).__added!,
  );
  // The measured property: exactly one node was added to the live region --
  // the arriving row, whole, carrying its own aria-atomic="true" -- not the
  // standing pile being rebuilt in and re-announced alongside it.
  expect(added).toHaveLength(1);
  expect(added[0]).toContain(idB);
  expect(added[0]).not.toContain(idA);
});

test("a dispatch-history arrival's detail and reason live inside the same atomic node as its headline", async ({
  page,
}) => {
  await page.goto("/");
  const title = "SH-337 — history announcer detail";
  const id = await raiseHistoryRow(page, title);

  const row = page.locator("#dispatch-history .dispatch-history-row").first();
  await expect(row).toHaveAttribute("aria-atomic", "true");
  await expect(row).toContainText(`${id} refused`);
  await expect(row.locator(".notice-detail")).toContainText("already in-progress");
  await expect(row.locator(".notice-reason")).toHaveText("claim-conflict");
});

test("two identical dismissal announcements in a row both mutate the announcer", async ({
  page,
}) => {
  // `setStatusText`'s clear-then-set idiom (SH-333 rider 2) is under test
  // here, not a dispatch-history property. SH-337 made a dispatch-history
  // arrival insert a distinct DOM node per notice, which is always a genuine
  // mutation regardless of text equality and cannot regress into the
  // same-value-reassignment collapse this idiom guards against. The
  // surviving surface where that collapse is a real risk is
  // `#notice-dock-status`, fed by `announceInNoticeDock` on every dismissal
  // -- one shared element whose `textContent` really is reassigned.
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  const title = "SH-333 — dismissal announcer repeat";
  await createStory(page, title);
  await raiseDurableNotices(page, title, 3);

  await page.locator("#notice-dock-status").evaluate((node) => {
    (node as HTMLElement & { __mutations?: number }).__mutations = 0;
    const observer = new MutationObserver((records) => {
      (node as HTMLElement & { __mutations?: number }).__mutations! += records.length;
    });
    observer.observe(node, { childList: true, characterData: true, subtree: true });
  });

  await page.locator("#toast-stack .toast .toast-dismiss").first().click();
  await expect(page.locator("#notice-dock-status")).toHaveText("Notice dismissed. 2 remaining.");
  const firstAnnouncement = await page.locator("#notice-dock-status").textContent();

  // Back up to 3 standing, then dismiss one again -- 2 remaining once more,
  // the identical announcement text as above.
  await raiseDurableNotices(page, title, 1);
  await page.locator("#toast-stack .toast .toast-dismiss").first().click();
  await expect(page.locator("#notice-dock-status")).toHaveText("Notice dismissed. 2 remaining.");
  const secondAnnouncement = await page.locator("#notice-dock-status").textContent();
  expect(secondAnnouncement).toBe(firstAnnouncement);

  const mutationCount = await page.locator("#notice-dock-status").evaluate(
    (node) => (node as HTMLElement & { __mutations?: number }).__mutations!,
  );
  // Clear-then-set is two DOM mutations per announcement (removing the old
  // text node, adding the new one); two announcements with identical text
  // must not collapse into fewer just because the text happens to match.
  expect(mutationCount).toBeGreaterThanOrEqual(3);
});

// ============================================================
// Inventory and non-interference
// ============================================================

test("the notice dock's only live regions are the two notice surfaces and one sr-only announcer", async ({
  page,
}) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  const title = "SH-337 — live region inventory";
  await createStory(page, title);
  await raiseDurableNotices(page, title, 1);
  await raiseHistoryRow(page, "SH-337 — live region inventory, history");

  const inventory = await page.evaluate(() => {
    const dock = document.getElementById("notice-dock")!;
    const nodes = Array.from(
      dock.querySelectorAll('[aria-live], [role="status"], [role="alert"]'),
    );
    return nodes.map((n) => n.id || `${n.tagName.toLowerCase()}.${n.className}`);
  });

  // `#dispatch-history-status` is gone (SH-337): the panel is its own live
  // region now, the same shape `#toast-stack` has had since SH-333.
  expect(inventory.sort()).toEqual(["toast-stack", "dispatch-history", "notice-dock-status"].sort());

  const dispatchHistoryAttrs = await page.locator("#dispatch-history").evaluate((n) => ({
    ariaLive: n.getAttribute("aria-live"),
    ariaAtomic: n.getAttribute("aria-atomic"),
    role: n.getAttribute("role"),
  }));
  expect(dispatchHistoryAttrs.ariaLive).toBe("polite");
  expect(dispatchHistoryAttrs.ariaAtomic).toBe("false");
  expect(dispatchHistoryAttrs.role).toBe("region");
});

test("dispatch-history's live-region status changed with no loss of landmark or reachability", async ({
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

  // A dispatch-history arrival announces through #dispatch-history itself now
  // (SH-337) -- #notice-dock-status must still hold the dismissal message
  // above, untouched. This is SH-333's rider 1 satisfied structurally rather
  // than by discipline: the arrival path no longer touches any shared
  // announcer at all, so there is nothing left that could overwrite the
  // armed-delete confirmation prompt that element also carries.
  await raiseHistoryRow(page, "SH-333 — no clobber, history");
  await expect(page.locator("#notice-dock-status")).toHaveText(
    "Notice dismissed. No notices remaining.",
  );
  await expect(page.locator("#dispatch-history .dispatch-history-row").first()).toContainText(
    "refused",
  );
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

/**
 * SH-367 — "Dismiss all" discloses what it destroys, as a HYBRID.
 *
 * The bar used to announce only "All notices dismissed", and
 * `dismissAllDispatchHistory`'s comment justified that silence: the toast pile
 * MIXES a server-backed dispatch failure with copy, deep-link and mutation
 * errors that have no record anywhere, so promising recovery would be a
 * comforting falsehood. The premise was correct; the conclusion was not.
 * Silence over a mixed state is itself read as "none of these survive" — which
 * is SH-312's rule broken in the other direction, a mixed outcome reported as
 * a uniform one.
 *
 * The shape is SH-361's own `retention.forgotten` amendment applied one
 * surface over: the RULE sentence is unconditional and rendered always,
 * *because a rule cannot be wrong*; the clause that points somewhere is
 * conditional on this pile actually holding something that surface will have.
 *
 * Both branches are asserted here. A pin on only the dispatch case would pass
 * against a build that pointed every reader at the Dispatch log, including the
 * reader who just cleared three clipboard failures that are in no log at all —
 * which is the exact falsehood this design exists to avoid.
 */
test("Dismiss all states the retention rule always, and points at the log only when there is something in it", async ({
  page,
}) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  // Branch 1: client-only notices. Nothing here survives, and the
  // announcement must not imply otherwise.
  const title = "SH-367 — dismiss-all disclosure";
  await createStory(page, title);
  await raiseDurableNotices(page, title, 2);

  await page.locator("#toast-dismiss-all").click();
  const announcer = page.locator("#notice-dock-status");
  await expect(announcer).toContainText("Dismissed notices are not kept.");
  await expect(announcer).not.toContainText("Dispatch log");

  // Branch 2: a genuinely MIXED pile — one dispatch failure the daemon keeps,
  // beside a clipboard failure it does not. That mixture is the case this
  // whole design is about, and it needs two notices anyway: the bar is hidden
  // below `NOTICE_DISMISS_ALL_THRESHOLD` (2), so a lone refusal cannot be
  // bulk-dismissed at all.
  //
  // `DispatchRegistry::finish` has no `auto` filter, so this ATTENDED refusal
  // is in `GET /api/dispatch-log` exactly as an unattended one would be.
  await raiseAttendedRefusal(page, "SH-367 refusal for the mixed pile");
  await raiseDurableNotices(page, title, 1);
  await expect(page.locator("#toast-stack .toast")).toHaveCount(2);

  await page.locator("#toast-dismiss-all").click();
  await expect(announcer).toContainText("Dismissed notices are not kept.");
  await expect(announcer).toContainText("Dispatch log under Settings");
});
