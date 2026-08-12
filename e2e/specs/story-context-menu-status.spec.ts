import { test, expect } from "@playwright/test";
import { cleanUpCreatedStories, deleteStory, seedToken } from "./support";

/**
 * Exercises SH-197's Set Status submenu: every configured state but the
 * story's own, with the board's own state color dot; picking one applies
 * it the same way dragging a card would (`setStoryState()` calls
 * `moveStory()` verbatim for an open story). A closed story's submenu is
 * narrower -- only OPEN states, since the server refuses every mutation of
 * a closed story outright -- and picking a non-default one chases a
 * `/reopen` with an explicit `/move` so it lands exactly where asked
 * rather than wherever `/reopen` alone would default to.
 *
 * This spec creates and deletes its own stories rather than touching the
 * "Alpha Project" fixture, whose exact two-story shape other specs
 * (filter-persistence.spec.ts, column-visibility.spec.ts) assert on
 * byte-for-byte per run-e2e.sh's own comment.
 */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await expect(page.locator("#board-view")).toBeVisible();
});

async function createStory(page: import("@playwright/test").Page, title: string) {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  const card = page.locator('.column[data-state="todo"] .card', {
    hasText: title,
  });
  await expect(card).toBeVisible();
  return card;
}

/** Moves `title`'s story to `stateSlug` via the drawer's own State select
 * -- not scoped to any one column, since it's also used to move a story
 * back out of wherever an earlier call left it. */
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

async function openSetStatus(
  page: import("@playwright/test").Page,
  card: import("@playwright/test").Locator,
) {
  await card.click({ button: "right" });
  const setStatus = page.locator(".ctxmenu-item", { hasText: "Set Status" });
  await expect(setStatus).toBeVisible();
  await setStatus.click();
  const submenu = page.locator(".ctxmenu-sub");
  await expect(submenu).toBeVisible();
  return submenu;
}

test("the submenu omits the story's own state and shows every other configured one", async ({
  page,
}) => {
  const title = "SH-197 context menu — submenu contents";
  const card = await createStory(page, title);

  const submenu = await openSetStatus(page, card);
  await expect(submenu.locator(".ctxmenu-item", { hasText: /^todo$/ })).toHaveCount(0);
  await expect(submenu.locator(".ctxmenu-item", { hasText: /^in-progress$/ })).toBeVisible();
  await expect(submenu.locator(".ctxmenu-item", { hasText: /^done$/ })).toBeVisible();
  await expect(submenu.locator(".ctxmenu-item .dot")).not.toHaveCount(0);

  await page.keyboard.press("Escape");
  await page.keyboard.press("Escape");
  await deleteStory(page, title);
});

test("picking a state moves the card between columns, same as a drag", async ({
  page,
}) => {
  const title = "SH-197 context menu — pick a status";
  const card = await createStory(page, title);

  const submenu = await openSetStatus(page, card);
  await submenu.locator(".ctxmenu-item", { hasText: /^in-progress$/ }).click();

  await expect(page.locator(".ctxmenu")).toHaveCount(0);
  await expect(
    page.locator('.column[data-state="in-progress"] .card', { hasText: title }),
  ).toBeVisible();
  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: title }),
  ).toHaveCount(0);

  await moveToState(page, title, "todo");
  await deleteStory(page, title);
});

test("keyboard: ArrowRight opens the submenu and focuses its first item, Enter selects", async ({
  page,
}) => {
  const title = "SH-197 context menu — keyboard submenu";
  const card = await createStory(page, title);

  await card.click({ button: "right" });
  // End then ArrowUp, not a raw .focus() on the item: only real arrow/
  // Home/End navigation updates the menu's own roving-focus bookkeeping
  // (storyMenuFocusIndex), which is what ArrowRight below reads to know
  // which item is "current". Set Status sits directly above Delete, the
  // model's actual last entry -- ArrowUp once from the end reaches it
  // regardless of what (if anything) comes after it.
  await page.keyboard.press("End");
  await page.keyboard.press("ArrowUp");
  const setStatus = page.locator(".ctxmenu-item", { hasText: "Set Status" });
  await expect(setStatus).toBeFocused();
  await page.keyboard.press("ArrowRight");

  const submenu = page.locator(".ctxmenu-sub");
  await expect(submenu).toBeVisible();
  const firstItem = submenu.locator(".ctxmenu-item").first();
  await expect(firstItem).toBeFocused();

  await page.keyboard.press("ArrowDown");
  const secondItem = submenu.locator(".ctxmenu-item").nth(1);
  await expect(secondItem).toBeFocused();
  const secondLabel = await secondItem.textContent();

  await page.keyboard.press("Enter");
  await expect(page.locator(".ctxmenu")).toHaveCount(0);
  await expect(
    page.locator('.column[data-state="' + secondLabel!.trim() + '"] .card', {
      hasText: title,
    }),
  ).toBeVisible();

  await moveToState(page, title, "todo");
  await deleteStory(page, title);
});

test("ArrowLeft closes just the submenu and returns focus to Set Status, without changing anything", async ({
  page,
}) => {
  const title = "SH-197 context menu — ArrowLeft closes submenu only";
  const card = await createStory(page, title);

  await card.click({ button: "right" });
  // See the previous test's identical comment: ArrowUp once from the end
  // reaches Set Status regardless of what comes after it (Delete, today).
  await page.keyboard.press("End");
  await page.keyboard.press("ArrowUp");
  const setStatus = page.locator(".ctxmenu-item", { hasText: "Set Status" });
  await expect(setStatus).toBeFocused();
  await page.keyboard.press("ArrowRight");
  await expect(page.locator(".ctxmenu-sub")).toBeVisible();

  await page.keyboard.press("ArrowLeft");
  await expect(page.locator(".ctxmenu-sub")).toHaveCount(0);
  // The main menu is still open -- ArrowLeft backed out of the submenu,
  // not the whole thing.
  await expect(page.locator(".ctxmenu")).toBeVisible();
  await expect(setStatus).toBeFocused();

  await page.keyboard.press("Escape");
  await expect(page.locator(".ctxmenu")).toHaveCount(0);
  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: title }),
  ).toBeVisible();

  await deleteStory(page, title);
});

test("a closed story's submenu offers only OPEN states, and reopening to a non-default one lands exactly there", async ({
  page,
}) => {
  const title = "SH-197 context menu — closed story reopen chain";
  await createStory(page, title);
  await moveToState(page, title, "done");

  const closedCard = page.locator('.column[data-state="done"] .card', {
    hasText: title,
  });
  const submenu = await openSetStatus(page, closedCard);

  // Only OPEN states -- "done" itself and any other CLOSED state are
  // unreachable from here (closed -> closed is not expressible: the server
  // refuses every mutation of a closed story outright).
  await expect(submenu.locator(".ctxmenu-item", { hasText: /^done$/ })).toHaveCount(0);
  const target = submenu.locator(".ctxmenu-item", { hasText: /^in-progress$/ });
  await expect(target).toBeVisible();

  // "in-progress" is deliberately NOT this project's default open state
  // (todo is) -- picking it must chase the /reopen with an explicit /move,
  // not silently land on the default the way a bare reopen would.
  await target.click();
  await expect(page.locator(".ctxmenu")).toHaveCount(0);
  await expect(
    page.locator('.column[data-state="in-progress"] .card', { hasText: title }),
  ).toBeVisible();
  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: title }),
  ).toHaveCount(0);

  await moveToState(page, title, "todo");
  await deleteStory(page, title);
});
