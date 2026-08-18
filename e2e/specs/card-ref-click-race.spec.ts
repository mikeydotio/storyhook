import { test, expect } from "./support";
import {
  awaitNoOverlay,
  cleanUpCreatedStories,
  createStory,
  deleteBlockedStory,
  deleteStory,
  heldReadDeadlineMs,
  holdFetch,
  openProject,
  projectSlug,
  requiredEnv,
  seedToken,
} from "./support";

/**
 * SH-399 — `.rel-id` (storyRef()'s button, opening a DIFFERENT story's
 * drawer) is the residual SH-397 named but deliberately did not fix: the
 * card body is immune to a mid-click re-render (`.card * { pointer-events:
 * none }`), but `.rel-id` had to be punched back through with
 * `pointer-events: auto` since it needs genuine per-target interactivity --
 * `storyRef()`'s own `e.stopPropagation()` depends on the click actually
 * reaching it -- so it stayed exposed to the identical race.
 *
 * Root cause is the same one SH-397 found: `populateCard()` used to
 * `clear()` and rebuild every child on every render, unconditionally,
 * whether or not anything the card renders actually changed. A `/data`
 * reply landing between a real mousedown and mouseup on `.rel-id` destroyed
 * it out from under the pointer, and per the UI Events click-dispatch
 * algorithm a `mousedown` target disconnected before `mouseup` means no
 * `click` fires anywhere -- not even at `.card`, since `.rel-id`'s own
 * listener never runs and its `stopPropagation()` never happens.
 *
 * SH-399's fix makes the rebuild itself conditional: `populateCard` now
 * skips `clear()`+rebuild entirely when what the card would render hasn't
 * changed. This spec deliberately drives that exact "no-op for THIS card"
 * case -- a mutation that changes nothing the worker's card renders -- so
 * it proves the actual claim ("an unrelated render must not touch this
 * card's DOM at all"), not merely "some click survived some render."
 * `drawer-open-race.spec.ts` is the template for the race-forcing
 * technique (`holdFetch` gates a genuine `/data` reply so it lands exactly
 * between a real `page.mouse.down()` and `page.mouse.up()`).
 */

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  // Past this suite's own maximum patience (SH-347) -- the test below holds
  // `/data`, and the page's own read deadline must not race it.
  await page.goto(`/?boardFetchTimeoutMs=${heldReadDeadlineMs()}`);
  await openProject(page, "Alpha Project");
});

cleanUpCreatedStories("Alpha Project");

/** Records `kind` from the currently-open drawer's story onto `otherId`, via
 * the Relationships section's inline-add form -- the same shape
 * `status-flags.spec.ts`'s own `addRelation` and `card-blockers.spec.ts`
 * use inline. Assumes a drawer is already open. */
async function addRelation(
  page: import("@playwright/test").Page,
  kind: string,
  otherId: string,
) {
  await page.locator('input[placeholder="Story ID (e.g. SH-2)"]').fill(otherId);
  await page.locator("#drawer-body .inline-add select").selectOption(kind);
  await page
    .locator("#drawer-body .inline-add button", { hasText: "Add" })
    .click();
  await expect(page.locator(".rel-row", { hasText: otherId })).toBeVisible();
}

/** Seeds a blocker `B` and a worker `W` blocked-by `B`, returning both ids.
 * SH-407 display-promotes `W`'s card out of "todo" and into "blocked" the
 * instant the edge is recorded -- callers must locate it there. */
async function seedBlockedPair(
  page: import("@playwright/test").Page,
  blockerTitle: string,
  workerTitle: string,
) {
  const blockerId = await createStory(page, blockerTitle);
  const workerId = await createStory(page, workerTitle);

  await page
    .locator('.column[data-state="todo"] .card', { hasText: workerTitle })
    .click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await addRelation(page, "blocked-by", blockerId);
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  // Two settling waits a manual mouse.down/up choreography needs that
  // locator.click() gets for free via its own actionability checks:
  //
  // 1. `#drawer-backdrop` fades out on a timer AFTER closeDrawer() removes
  //    `.open` (SH-302) -- `#drawer` losing `.open` does not mean the
  //    backdrop has finished fading to `hidden`, and `.backdrop` is
  //    `position: fixed; inset: 0`, so it still intercepts a raw
  //    coordinate-based mouse event underneath it until it does.
  //    `awaitNoOverlay` is this file's own helper for exactly that wait.
  // 2. SH-407's display-promotion moves the worker's card into "blocked"
  //    via playFlip()'s FLIP animation (up to 320ms) -- a bounding box
  //    grabbed before that settles is stale by the time a raw mouse event
  //    reaches it.
  await awaitNoOverlay(page);
  const workerCard = page.locator(".card", { hasText: workerTitle });
  await expect(workerCard).not.toHaveClass(/moving/);

  return { blockerId, workerId };
}

test("a /data reply that changes nothing this card renders does not swallow a click on its blocked-by ref (SH-399)", async ({
  page,
  request,
}) => {
  const blockerTitle = "SH-399 forced race — the blocker";
  const workerTitle = "SH-399 forced race — the blocked story";
  const { blockerId, workerId } = await seedBlockedPair(
    page,
    blockerTitle,
    workerTitle,
  );

  const workerCard = page.locator('.column[data-state="blocked"] .card', {
    hasText: workerTitle,
  });
  const ref = workerCard.locator(".flag-blocked .rel-id");
  await expect(ref).toHaveText(blockerId);
  const box = await ref.boundingBox();
  if (!box) {
    throw new Error(`"${workerTitle}"'s .rel-id has no box to click`);
  }

  // Gate the *next* `/data` reply so it lands exactly where the race needs
  // it -- between mousedown and mouseup.
  const held = await holdFetch(
    page,
    (url) => url.pathname.endsWith("/data"),
    () => true,
  );

  // A real mutation through the daemon's own API drives the `repo-changed`
  // SSE this held fetch answers -- deliberately a comment, which changes
  // nothing `workerCard` renders (not its title, labels, assignee, type,
  // blocked-by set, or awaiting reason), so the render this forces is a
  // genuine no-op for this card.
  const slug = await projectSlug(request, "Alpha Project");
  const commented = await request.post(
    `/api/repos/${encodeURIComponent(slug)}/story/${encodeURIComponent(workerId)}/comment`,
    {
      headers: {
        "X-Storyhook": "1",
        "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
      },
      data: { text: "SH-399 forced-race trigger" },
    },
  );
  if (!commented.ok()) {
    throw new Error(
      `POST .../comment answered ${commented.status()}: ${await commented.text()} -- ` +
        "this spec depends on it landing, to drive the held /data reply",
    );
  }
  await held.taken;

  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  // Releases the held reply -- fetchData()'s success handler runs
  // renderAll() synchronously. The mouse is still down; on the unfixed
  // tree, populateCard() would have destroyed .rel-id under it regardless
  // of whether anything actually changed.
  await held.deliver();
  await page.mouse.up();

  // The BLOCKER's id, not the worker's: proves the click reached .rel-id
  // specifically (not merely `.card`, which would open the worker's own
  // drawer) and that storyRef()'s stopPropagation() still ran.
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(page.locator("#drawer-id")).toHaveText(blockerId);

  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  await deleteBlockedStory(page, workerTitle);
  await deleteStory(page, blockerTitle);
});

/**
 * Control for the test above: the identical mouse choreography and the
 * identical no-op mutation, but the click is driven with no interposed
 * `/data` hold at all -- proves the down/up-split technique itself opens
 * the blocker's drawer normally, so a pass above is never mistaken for an
 * artifact of how the click was driven rather than of the race removed.
 */
test("the same down/up choreography opens the blocker's drawer when nothing re-renders in between", async ({
  page,
}) => {
  const blockerTitle = "SH-399 forced race — control, the blocker";
  const workerTitle = "SH-399 forced race — control, the blocked story";
  const { blockerId } = await seedBlockedPair(page, blockerTitle, workerTitle);

  const workerCard = page.locator('.column[data-state="blocked"] .card', {
    hasText: workerTitle,
  });
  const ref = workerCard.locator(".flag-blocked .rel-id");
  const box = await ref.boundingBox();
  if (!box) {
    throw new Error(`"${workerTitle}"'s .rel-id has no box to click`);
  }

  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.up();

  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(page.locator("#drawer-id")).toHaveText(blockerId);

  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  await deleteBlockedStory(page, workerTitle);
  await deleteStory(page, blockerTitle);
});

/**
 * The same conditional-rebuild mechanism's free side effect: `.rel-id` is a
 * real, tabbable `<button>` (`storyRef()` is shared with the drawer, where
 * it must be), so it is in the board's tab order, but nothing in
 * `focusedCardId`/`syncRoving` tracks it -- before SH-399, ANY render
 * destroyed and rebuilt it, silently dumping keyboard focus to `<body>`.
 * Since a no-op render under the fix leaves the node untouched, focus
 * survives for free. Driven via `.focus()` rather than a real Tab
 * keypress, deliberately: this asserts the board-side mechanism (does the
 * node survive) directly, without also depending on WebKit's own
 * Full-Keyboard-Access-gated Tab order (a real Tab press onto a `<button>`
 * needs `fullKeyboardAccess()`; a locator's own `.focus()` call does not,
 * since it invokes the DOM `focus()` method rather than simulating
 * traversal), so this test runs unconditionally on every project.
 */
test("focus on a card's blocked-by ref survives a /data reply that changes nothing this card renders", async ({
  page,
  request,
}) => {
  const blockerTitle = "SH-399 forced race — focus, the blocker";
  const workerTitle = "SH-399 forced race — focus, the blocked story";
  const { workerId } = await seedBlockedPair(page, blockerTitle, workerTitle);

  const workerCard = page.locator('.column[data-state="blocked"] .card', {
    hasText: workerTitle,
  });
  const ref = workerCard.locator(".flag-blocked .rel-id");
  await ref.focus();
  await expect(ref).toBeFocused();

  const slug = await projectSlug(request, "Alpha Project");
  const commented = await request.post(
    `/api/repos/${encodeURIComponent(slug)}/story/${encodeURIComponent(workerId)}/comment`,
    {
      headers: {
        "X-Storyhook": "1",
        "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
      },
      data: { text: "SH-399 focus-preservation trigger" },
    },
  );
  if (!commented.ok()) {
    throw new Error(
      `POST .../comment answered ${commented.status()}: ${await commented.text()}`,
    );
  }

  // The comment lands via SSE -> a real board refetch and re-render;
  // `toBeFocused()` is itself a web-first assertion and polls for that
  // round trip rather than needing an outer retry wrapper.
  await expect(ref).toBeFocused();

  await deleteBlockedStory(page, workerTitle);
  await deleteStory(page, blockerTitle);
});
