import { test, expect } from "@playwright/test";
import { deleteStory, projectSlug, seedToken } from "./support";

/**
 * Exercises SH-197's story context menu: right-click (or a keyboard-raised
 * `contextmenu`, once the roving tabindex from the previous commit makes a
 * card/row focusable) opens a `role="menu"` popover of per-story actions.
 * This file covers the menu shell itself and its first item group --
 * Copy ID, Copy URL, Copy Description -- built fresh in `openStoryMenu`
 * (`src/web_dashboard.html`). Later groups (Dispatch, Set Status, Delete)
 * have their own spec files.
 *
 * This spec creates and deletes its own stories rather than touching the
 * "Alpha Project" fixture, whose exact two-story shape other specs
 * (filter-persistence.spec.ts, column-visibility.spec.ts) assert on
 * byte-for-byte per run-e2e.sh's own comment.
 */

test.beforeEach(async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await seedToken(page);
  await page.goto("/");
  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await expect(page.locator("#board-view")).toBeVisible();
});

async function createStory(
  page: import("@playwright/test").Page,
  title: string,
  description?: string,
) {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  if (description) await page.locator("#create-description").fill(description);
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  const card = page.locator('.column[data-state="todo"] .card', {
    hasText: title,
  });
  await expect(card).toBeVisible();
  return card;
}

test("right-click a card shows Copy ID, Copy URL, Copy Description, in that order", async ({
  page,
}) => {
  const title = "SH-197 context menu — item order";
  const card = await createStory(page, title, "a description");

  await card.click({ button: "right" });
  const menu = page.locator(".ctxmenu");
  await expect(menu).toBeVisible();
  await expect(menu).toHaveAttribute("role", "menu");

  // 6, not 3: Alpha has a checkout, so the Dispatch group (its own spec,
  // story-context-menu-dispatch.spec.ts) follows the copy group, and Set
  // Status (its own spec, story-context-menu-status.spec.ts) follows that.
  const items = menu.locator(".ctxmenu-item");
  await expect(items).toHaveCount(6);
  await expect(items.nth(0)).toHaveText("Copy ID");
  await expect(items.nth(1)).toHaveText("Copy URL");
  await expect(items.nth(2)).toHaveText("Copy Description");
  await expect(items.nth(3)).toHaveText("Dispatch");
  await expect(items.nth(4)).toHaveText("Dispatch Auto");
  // toContainText, not toHaveText: the item's own submenu arrow (▸) is
  // part of its text content too.
  await expect(items.nth(5)).toContainText("Set Status");
  await expect(menu.locator(".ctxmenu-sep")).toHaveCount(2);

  await page.keyboard.press("Escape");
  await expect(menu).toHaveCount(0);
  await deleteStory(page, title);
});

test("Copy ID puts the story's id on the clipboard", async ({ page }) => {
  const title = "SH-197 context menu — copy id";
  const card = await createStory(page, title);
  const id = await card.getAttribute("data-id");

  await card.click({ button: "right" });
  await page.locator(".ctxmenu-item", { hasText: "Copy ID" }).click();
  await expect(page.locator("#toast-stack .toast.success")).toContainText(
    "ID copied",
  );

  const clipboard = await page.evaluate(() => navigator.clipboard.readText());
  expect(clipboard).toBe(id);

  await deleteStory(page, title);
});

test("Copy URL puts the real deep link on the clipboard", async ({
  page,
  request,
}) => {
  const title = "SH-197 context menu — copy url";
  const card = await createStory(page, title);
  const id = await card.getAttribute("data-id");
  const slug = await projectSlug(request, "Alpha Project");

  await card.click({ button: "right" });
  await page.locator(".ctxmenu-item", { hasText: "Copy URL" }).click();
  await expect(page.locator("#toast-stack .toast.success")).toContainText(
    "URL copied",
  );

  const clipboard = await page.evaluate(() => navigator.clipboard.readText());
  const url = new URL(clipboard);
  expect(url.origin + url.pathname).toBe(
    new URL(page.url()).origin + new URL(page.url()).pathname,
  );
  expect(url.searchParams.get("project")).toBe(slug);
  expect(url.searchParams.get("story")).toBe(id);

  // The copied link actually opens the story it names.
  await page.goto(clipboard);
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(page.locator("#drawer-id")).toHaveText(id!);

  await page.locator("#drawer-close").click();
  await deleteStory(page, title);
});

test("Copy Description is disabled with no description, and copies the text when there is one", async ({
  page,
}) => {
  const bare = "SH-197 context menu — no description";
  const bareCard = await createStory(page, bare);

  await bareCard.click({ button: "right" });
  const disabledItem = page.locator(".ctxmenu-item", {
    hasText: "Copy Description",
  });
  await expect(disabledItem).toHaveAttribute("aria-disabled", "true");
  // force: true -- Playwright's own actionability check already refuses to
  // click an aria-disabled element, which is the real assertion; forcing
  // it through proves the app's own handler ALSO refuses, in case a
  // keyboard Enter/Space ever reached it a different way.
  await disabledItem.click({ force: true });
  // Disabled means disabled: no toast, no clipboard write, menu stays open.
  await expect(page.locator("#toast-stack .toast")).toHaveCount(0);
  await expect(page.locator(".ctxmenu")).toBeVisible();
  await page.keyboard.press("Escape");

  const withDescription = "SH-197 context menu — has a description";
  const card = await createStory(
    page,
    withDescription,
    "This story has a description",
  );
  await card.click({ button: "right" });
  const enabledItem = page.locator(".ctxmenu-item", {
    hasText: "Copy Description",
  });
  await expect(enabledItem).not.toHaveAttribute("aria-disabled", "true");
  await enabledItem.click();
  await expect(page.locator("#toast-stack .toast.success")).toContainText(
    "Description copied",
  );
  const clipboard = await page.evaluate(() => navigator.clipboard.readText());
  expect(clipboard).toBe("This story has a description");

  await deleteStory(page, bare);
  await deleteStory(page, withDescription);
});

test("copying still works when navigator.clipboard doesn't exist (the tailnet http case)", async ({
  page,
  context,
}) => {
  const title = "SH-197 context menu — clipboard fallback";
  // navigator.clipboard is a secure-context-only API; the daemon's real
  // tailnet URL is plain http, where it's simply undefined. Deleting it
  // here forces copyText() down its document.execCommand("copy") path.
  await page.addInitScript(() => {
    Object.defineProperty(window.navigator, "clipboard", {
      value: undefined,
      configurable: true,
    });
  });
  // No home-card click needed: bootstrap() restores the last-viewed
  // project straight from localStorage on reload, landing directly back on
  // Alpha's board.
  await page.reload();
  await expect(page.locator("#board-view")).toBeVisible();

  const card = await createStory(page, title);
  const id = await card.getAttribute("data-id");

  await card.click({ button: "right" });
  await page.locator(".ctxmenu-item", { hasText: "Copy ID" }).click();
  await expect(page.locator("#toast-stack .toast.success")).toContainText(
    "ID copied",
  );

  // The acting page has no navigator.clipboard to read back with -- a
  // second page in the same browser context does (this addInitScript was
  // only ever registered on `page`), and the OS/browser clipboard the
  // fallback wrote to is shared across both. Navigated to the dashboard's
  // own origin, not about:blank: an opaque-origin page doesn't expose
  // navigator.clipboard at all, regardless of granted permissions.
  const reader = await context.newPage();
  await reader.goto("/");
  const clipboard = await reader.evaluate(() => navigator.clipboard.readText());
  expect(clipboard).toBe(id);
  await reader.close();

  await deleteStory(page, title);
});

test("outside click and Escape dismiss the menu without side effects", async ({
  page,
}) => {
  const title = "SH-197 context menu — dismissal";
  const card = await createStory(page, title);

  await card.click({ button: "right" });
  await expect(page.locator(".ctxmenu")).toBeVisible();
  await page.mouse.click(10, 10);
  await expect(page.locator(".ctxmenu")).toHaveCount(0);

  await card.click({ button: "right" });
  await expect(page.locator(".ctxmenu")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.locator(".ctxmenu")).toHaveCount(0);
  // Escape restores focus to the card that opened the menu.
  await expect(card).toBeFocused();

  await deleteStory(page, title);
});

test("a right-click near the viewport edge keeps the menu on screen", async ({
  page,
}) => {
  const title = "SH-197 context menu — clamped to viewport";
  // Width stays above the 768px responsive breakpoint (desktop layout, so
  // the topbar's #new-story-btn stays where createStory() expects it);
  // height is short enough that the first column's first card -- always
  // near the top -- has the viewport's bottom edge close behind it, so a
  // right-click on its own lower edge needs placeMenu()'s clamp to avoid
  // spilling off-screen.
  await page.setViewportSize({ width: 900, height: 320 });
  const card = await createStory(page, title);
  const box = await card.boundingBox();
  expect(box).toBeTruthy();

  await card.click({
    button: "right",
    position: { x: 5, y: box!.height - 2 },
  });
  const menuBox = await page.locator(".ctxmenu").boundingBox();
  expect(menuBox).toBeTruthy();
  const viewport = page.viewportSize()!;
  expect(menuBox!.x).toBeGreaterThanOrEqual(0);
  expect(menuBox!.y).toBeGreaterThanOrEqual(0);
  expect(menuBox!.x + menuBox!.width).toBeLessThanOrEqual(viewport.width);
  expect(menuBox!.y + menuBox!.height).toBeLessThanOrEqual(viewport.height);

  await page.keyboard.press("Escape");
  await page.setViewportSize({ width: 1280, height: 720 });
  await deleteStory(page, title);
});

test("the menu works on a list-view row too", async ({ page }) => {
  const title = "SH-197 context menu — list view";
  await createStory(page, title);

  await page.locator('#view-toggle button[data-view="list"]').click();
  await expect(page.locator("#list-view")).toBeVisible();

  const row = page.locator("#list-body tr", { hasText: title });
  await row.click({ button: "right" });
  const menu = page.locator(".ctxmenu");
  await expect(menu).toBeVisible();
  // 6, not 3: see the item-order test's identical note.
  await expect(menu.locator(".ctxmenu-item")).toHaveCount(6);

  await page.keyboard.press("Escape");
  await page.locator('#view-toggle button[data-view="board"]').click();
  await deleteStory(page, title);
});
