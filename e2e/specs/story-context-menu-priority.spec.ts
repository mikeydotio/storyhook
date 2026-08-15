import { test, expect } from "@playwright/test";
import {
  cleanUpCreatedStories,
  deleteStory,
  focusMenuItemByLabel,
  latch,
  openProject,
  resolvedTokenColor,
  seedToken,
} from "./support";

/**
 * Exercises SH-310's Set Priority submenu: the daemon's own priority
 * vocabulary (`meta().priorities`) as a `role="menuitemradio"` group with
 * the story's current value checked -- picking one applies it the same way
 * the drawer's own Priority `<select>` does (`setStoryPriority()` calls
 * `runFieldMutation()` verbatim). Unlike Set Status, which omits the
 * story's own state, this submenu deliberately offers and marks the
 * current value: the board has no other way to name a story's priority
 * short of opening the drawer (see `priorityMenuItems`'s own comment).
 *
 * This spec creates and deletes its own stories rather than touching the
 * "Alpha Project" fixture, whose exact two-story shape other specs
 * (filter-persistence.spec.ts, column-visibility.spec.ts) assert on
 * byte-for-byte per run-e2e.sh's own comment. Cards are always located by
 * `hasText`, never by index or column position: the board's default sort
 * is priority descending, so changing a card's priority mid-test can
 * reorder its own column.
 */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

async function createStory(
  page: import("@playwright/test").Page,
  title: string,
  priority?: string,
) {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  if (priority) await page.locator("#create-priority").selectOption(priority);
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  const card = page.locator('.column[data-state="todo"] .card', {
    hasText: title,
  });
  await expect(card).toBeVisible();
  return card;
}

/** Moves `title`'s story to `stateSlug` through the drawer's own State
 * select -- the identical helper `story-context-menu-status.spec.ts` uses,
 * kept local rather than shared: this file's only use of it is to reach
 * and leave the one closed-story case, not a repeated pattern worth a
 * third copy in support.ts. */
async function moveToState(
  page: import("@playwright/test").Page,
  title: string,
  stateSlug: string,
) {
  await page.locator(".card", { hasText: title }).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await page.locator("#drawer-body select").first().selectOption(stateSlug);
  await expect(
    page.locator('.column[data-state="' + stateSlug + '"] .card', {
      hasText: title,
    }),
  ).toBeVisible();
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
}

const priorityMenu = (page: import("@playwright/test").Page) =>
  page.locator('.ctxmenu-sub[aria-label="Set priority"]');

/** The submenu's item labels, with the always-present (sometimes hidden)
 * `✓` stripped -- `.ctxmenu-check` is `visibility: hidden` on an unchecked
 * item, not absent, so its glyph is in every item's `textContent`. */
async function priorityLabels(
  page: import("@playwright/test").Page,
): Promise<string[]> {
  const texts = await priorityMenu(page).locator(".ctxmenu-item").allTextContents();
  return texts.map((t) => t.replace("✓", "").trim());
}

async function openSetPriority(
  page: import("@playwright/test").Page,
  card: import("@playwright/test").Locator,
) {
  await card.click({ button: "right" });
  const item = page.locator(".ctxmenu-item", { hasText: "Set Priority" });
  await expect(item).toBeVisible();
  await item.click();
  await expect(priorityMenu(page)).toBeVisible();
}

test("the submenu offers the daemon's whole priority vocabulary, with the story's own checked", async ({
  page,
}) => {
  const title = "SH-310 context menu — submenu contents";
  // No priority passed: a new story defaults to "none" (Priority::default,
  // src/domain.rs), which is the case decision 1 is actually about -- the
  // default is a named, choosable row here, not a dash.
  const card = await createStory(page, title);

  await openSetPriority(page, card);
  expect(await priorityLabels(page)).toEqual([
    "critical",
    "high",
    "medium",
    "low",
    "none",
  ]);

  const submenu = priorityMenu(page);
  await expect(submenu.locator(".ctxmenu-item")).toHaveCount(5);
  await expect(submenu.locator('[role="menuitemradio"]')).toHaveCount(5);
  await expect(submenu.locator('[aria-checked="true"]')).toHaveCount(1);
  await expect(submenu.locator('[aria-checked="true"]')).toContainText("none");
  await expect(submenu.locator(".ctxmenu-item .dot")).toHaveCount(5);

  await page.keyboard.press("Escape");
  await page.keyboard.press("Escape");
  await deleteStory(page, title);
});

test("picking a priority repaints the card, and the menu and the drawer both report it next", async ({
  page,
}) => {
  const title = "SH-310 context menu — pick a priority";
  const card = await createStory(page, title);

  await openSetPriority(page, card);
  await priorityMenu(page).locator(".ctxmenu-item", { hasText: "critical" }).click();

  // Both nodes gone -- the submenu's own item click closes the whole story
  // menu (renderStorySubmenuNode's onClick), not just itself.
  await expect(page.locator(".ctxmenu")).toHaveCount(0);

  // The board, in the colour a reader actually sees -- not an optimistic
  // update (unlike moveStory's drag path), so this is a retrying poll.
  await expect
    .poll(() => card.evaluate((n) => getComputedStyle(n).borderLeftColor))
    .toBe(await resolvedTokenColor(page, "--p-critical"));

  // The menu reports it next.
  await openSetPriority(page, card);
  const submenu = priorityMenu(page);
  await expect(submenu.locator('[aria-checked="true"]')).toContainText("critical");
  await expect(
    submenu.locator(".ctxmenu-item", { hasText: "none" }),
  ).toHaveAttribute("aria-checked", "false");
  await page.keyboard.press("Escape");
  await page.keyboard.press("Escape");

  // And the drawer's own select -- the persistence proof, read through the
  // UI rather than the API. Scoped to #drawer-body: an unscoped `.field`
  // also matches the (hidden) create-story modal's own Priority field
  // (board-sort.spec.ts's own comment records the strict-mode violation
  // that causes).
  await card.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(
    page.locator("#drawer-body .field", { hasText: "Priority" }).locator("select"),
  ).toHaveValue("critical");
  await page.locator("#drawer-close").click();

  await deleteStory(page, title);
});

test("picking the priority the story already has issues no request at all", async ({
  page,
}) => {
  const title = "SH-310 context menu — no-op re-pick";
  const card = await createStory(page, title, "high");

  const writes: string[] = [];
  page.on("request", (r) => {
    if (r.method() === "POST" && new URL(r.url()).pathname.endsWith("/priority")) {
      writes.push(r.url());
    }
  });

  // Positive control: a counter that has never counted proves nothing.
  await openSetPriority(page, card);
  await priorityMenu(page).locator(".ctxmenu-item", { hasText: "critical" }).click();
  await expect
    .poll(() => card.evaluate((n) => getComputedStyle(n).borderLeftColor))
    .toBe(await resolvedTokenColor(page, "--p-critical"));
  expect(writes).toHaveLength(1);

  // The subject: re-open, confirm "critical" is the checked item, and pick
  // it again. The menu still closes (a submenu item's click always closes
  // the story menu, whether or not it turned out to be a no-op) -- only
  // the request is what must not happen.
  await openSetPriority(page, card);
  const submenu = priorityMenu(page);
  await expect(submenu.locator('[aria-checked="true"]')).toContainText("critical");
  await submenu.locator(".ctxmenu-item", { hasText: "critical" }).click();
  await expect(page.locator(".ctxmenu")).toHaveCount(0);
  expect(writes).toHaveLength(1);

  // A second positive control, which is also the deterministic barrier:
  // the page's one JS thread ran the no-op click's handler to completion
  // (a synchronous findStory + compare + early return, no promise
  // involved) before this click was even dispatched, so had the no-op
  // issued a request the listener above would already have recorded it.
  await openSetPriority(page, card);
  await priorityMenu(page).locator(".ctxmenu-item", { hasText: "low" }).click();
  await expect
    .poll(() => card.evaluate((n) => getComputedStyle(n).borderLeftColor))
    .toBe(await resolvedTokenColor(page, "--p-low"));
  expect(writes).toHaveLength(2);

  await deleteStory(page, title);
});

test("keyboard: ArrowRight opens Set Priority, and Enter applies the focused one", async ({
  page,
}) => {
  const title = "SH-310 context menu — keyboard submenu";
  const card = await createStory(page, title);

  await card.click({ button: "right" });
  const setPriority = await focusMenuItemByLabel(page, "Set Priority");
  await expect(setPriority).toBeFocused();
  await page.keyboard.press("ArrowRight");

  const submenu = priorityMenu(page);
  await expect(submenu).toBeVisible();
  // Item 0, not the checked one ("none") -- opening a submenu focuses its
  // first item regardless of which is checked, the same convention Set
  // Status already uses (storySubmenuFocusItem(0)); this story does not
  // change that.
  const firstItem = submenu.locator(".ctxmenu-item").first();
  await expect(firstItem).toBeFocused();
  await expect(firstItem).toContainText("critical");

  await page.keyboard.press("ArrowDown");
  const secondItem = submenu.locator(".ctxmenu-item").nth(1);
  await expect(secondItem).toBeFocused();
  await expect(secondItem).toContainText("high");

  await page.keyboard.press("Enter");
  await expect(page.locator(".ctxmenu")).toHaveCount(0);
  await expect
    .poll(() => card.evaluate((n) => getComputedStyle(n).borderLeftColor))
    .toBe(await resolvedTokenColor(page, "--p-high"));

  await deleteStory(page, title);
});

test("a closed story's menu offers Set Status but not Set Priority", async ({
  page,
}) => {
  const title = "SH-310 context menu — closed story";
  await createStory(page, title);
  await moveToState(page, title, "done");

  const closedCard = page.locator('.column[data-state="done"] .card', {
    hasText: title,
  });
  await closedCard.click({ button: "right" });
  await expect(
    page.locator(".ctxmenu-item", { hasText: "Set Status" }),
  ).toBeVisible();
  // Hidden, not disabled: the server refuses every edit of a closed story,
  // so there is nothing this item could ever do here.
  await expect(
    page.locator(".ctxmenu-item", { hasText: "Set Priority" }),
  ).toHaveCount(0);
  await page.keyboard.press("Escape");

  await moveToState(page, title, "todo");
  await deleteStory(page, title);
});

test("Set Priority is disabled while a move is still in flight", async ({
  page,
}) => {
  const title = "SH-310 context menu — disabled while pending";
  const card = await createStory(page, title);

  const gate = latch();
  await page.route(/\/story\/[^/]+\/move$/, async (route) => {
    if (route.request().method() !== "POST") {
      await route.continue();
      return;
    }
    const response = await route.fetch();
    await gate.held;
    await route.fulfill({ response });
  });

  // Drives the move through Set Status, which is `moveStory()`'s own path
  // and the one that sets `state.pending[id]` -- the clause under test.
  await card.click({ button: "right" });
  await page.locator(".ctxmenu-item", { hasText: "Set Status" }).click();
  const statusSubmenu = page.locator(".ctxmenu-sub");
  await expect(statusSubmenu).toBeVisible();
  await statusSubmenu.locator(".ctxmenu-item", { hasText: /^in-progress$/ }).click();

  const movedCard = page.locator('.column[data-state="in-progress"] .card', {
    hasText: title,
  });
  await expect(movedCard).toHaveClass(/pending/);

  // `.card.pending { pointer-events: none }` blocks a mouse right-click on
  // this exact card -- SH-197's own reasoning for why the roving-tabindex
  // keyboard path exists at all. A real Shift+F10 raises `contextmenu`
  // with `button: -1` and `clientX`/`clientY` both 0 (menuAnchor()'s own
  // detection for it); dispatching that event directly is what a keyboard
  // gesture on this exact card produces, not a bypass of it.
  await movedCard.evaluate((node) => {
    node.dispatchEvent(
      new MouseEvent("contextmenu", {
        bubbles: true,
        cancelable: true,
        button: -1,
        clientX: 0,
        clientY: 0,
      }),
    );
  });
  const menu = page.locator(".ctxmenu");
  await expect(menu).toBeVisible();
  await expect(
    menu.locator(".ctxmenu-item", { hasText: "Set Priority" }),
  ).toHaveAttribute("aria-disabled", "true");
  await expect(
    menu.locator(".ctxmenu-item", { hasText: "Set Status" }),
  ).toHaveAttribute("aria-disabled", "true");
  await page.keyboard.press("Escape");

  gate.release();
  await expect(movedCard).not.toHaveClass(/pending/);

  await movedCard.click({ button: "right" });
  await expect(
    page.locator(".ctxmenu-item", { hasText: "Set Priority" }),
  ).not.toHaveAttribute("aria-disabled", "true");
  await page.keyboard.press("Escape");

  await moveToState(page, title, "todo");
  await deleteStory(page, title);
});
