import { test, expect } from "./support";
import {
  cleanUpCreatedStories,
  createStory,
  deleteStory,
  heldReadDeadlineMs,
  holdFetch,
  openProject,
  projectSlug,
  requiredEnv,
  seedToken,
} from "./support";

/**
 * SH-397 — a card click WebKit intermittently swallowed, deep into a long
 * `make test-full` run: `deleteStory()`'s `card.click()` reported success,
 * yet `#drawer` never gained `open`, for the full 5s `expect` budget, and
 * never recovered on retry.
 *
 * Root cause (found by an adversarial review of the original static
 * diagnosis, verified here against the real render pipeline): `.card`'s own
 * node survives an unchanged-position board reconcile
 * (`reconcileColumnCards` reuses it, keyed by `data-id`), but
 * `populateCard()` unconditionally `clear()`s
 * and rebuilds every *child* on every render (`web_dashboard.html`,
 * `populateCard`) -- including `.card-title`, which is what a pointer
 * actually lands on for a short card, even though the click *listener* is
 * bound once on `.card` itself. Per the UI Events click-dispatch algorithm
 * (verified against Chromium's and WebKit's own behaviour, not merely
 * assumed): if the element under `mousedown` is disconnected before
 * `mouseup`, no `click` fires anywhere -- not even at an ancestor -- because
 * there is no longer a common inclusive ancestor connecting the two. A
 * `/data` reply landing in that narrow window (any mutation's own refetch,
 * the SSE-driven one, the 25s safety poll) destroys the pointer's target out
 * from under it. Playwright's hit-target check runs once, before the
 * gesture, against the node that existed then, and passes -- so the click
 * is reported successful even though nothing was ever dispatched. SH-422
 * later identified the whole-card boundary when reconciliation changes its
 * position; SH-401's press gate closes that broader case and
 * `card-reposition-click-race.spec.ts` witnesses it directly.
 *
 * This spec forces that exact race deterministically, through the real
 * production render path (`holdFetch` gates a genuine `/data` reply; no DOM
 * is touched by hand) rather than waiting out the rare organic occurrence,
 * and asserts the exact failure signature SH-397 reported. Before the fix
 * (`.card`'s presentational descendants made `pointer-events: none`, so a
 * click anywhere in the card body always hits `.card` itself, which a stable-
 * position reconcile never destroys) this test fails exactly like the two
 * reported occurrences: the assertion below times out with `#drawer` still
 * `class="drawer"`. After the fix it passes.
 */

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  // `boardFetchTimeoutMs` past this suite's own maximum patience (SH-347):
  // the one test below that holds `/data` must not race the page's own read
  // deadline for that hold. Harmless for the sibling test, which never holds
  // anything.
  await page.goto(`/?boardFetchTimeoutMs=${heldReadDeadlineMs()}`);
});

cleanUpCreatedStories("Alpha Project");

test("a /data reply landing between mousedown and mouseup does not swallow the card's click (SH-397)", async ({
  page,
  request,
}) => {
  await openProject(page, "Alpha Project");

  const title = "SH-397 forced race — click during a re-render";
  const id = await createStory(page, title);
  const card = page.locator('.column[data-state="todo"] .card', {
    hasText: title,
  });
  const box = await card.boundingBox();
  if (!box) {
    throw new Error(`"${title}"'s card has no box to click`);
  }

  // Gate the *next* `/data` reply so it lands exactly where the race needs
  // it -- between mousedown and mouseup -- rather than racing the machine's
  // own timing the way the organic occurrence did.
  const held = await holdFetch(
    page,
    (url) => url.pathname.endsWith("/data"),
    () => true,
  );

  // A real mutation, through the daemon's own API, is what drives the
  // `repo-changed` SSE this held fetch answers -- the same path a loaded
  // machine's own mutation-triggered refetch takes in production, not a
  // synthetic re-render.
  const slug = await projectSlug(request, "Alpha Project");
  const commented = await request.post(
    `/api/repos/${encodeURIComponent(slug)}/story/${encodeURIComponent(id)}/comment`,
    {
      headers: {
        "X-Storyhook": "1",
        "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
      },
      data: { text: "SH-397 forced-race trigger" },
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
  // Releases the held reply -- `fetchData()`'s success handler runs
  // `renderAll()` synchronously, and `populateCard()` clears and rebuilds
  // this card's children, before `deliver()` resolves. The mouse is still
  // down; the pointer's original target is gone.
  await held.deliver();
  await page.mouse.up();

  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(page.locator("#drawer-id")).toHaveText(id);

  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  await deleteStory(page, title);
});

/**
 * Control for the test above: the identical mouse choreography (a manual
 * `move` / `down` / `up` rather than `locator.click()`), but with no reply
 * interposed, so the pointer's target survives untouched. Proves the
 * technique itself -- splitting a click into separate down/up dispatches --
 * opens the drawer normally, so a pass above is never mistaken for an
 * artifact of how the click was driven rather than of the race removed.
 */
test("the same down/up choreography opens the drawer when nothing re-renders in between", async ({
  page,
}) => {
  await openProject(page, "Alpha Project");

  const title = "SH-397 forced race — control, no interposed render";
  const id = await createStory(page, title);
  const card = page.locator('.column[data-state="todo"] .card', {
    hasText: title,
  });
  const box = await card.boundingBox();
  if (!box) {
    throw new Error(`"${title}"'s card has no box to click`);
  }

  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.up();

  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(page.locator("#drawer-id")).toHaveText(id);

  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  await deleteStory(page, title);
});
