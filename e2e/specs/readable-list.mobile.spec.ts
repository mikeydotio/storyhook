import { test, expect } from "./support";
import type { Page } from "@playwright/test";
import {
  cleanUpCreatedStories,
  createStory,
  openProject,
  projectSlug,
  requiredEnv,
  seedToken,
} from "./support";

/**
 * SH-614: at the dashboard's <=768px breakpoint, List is a semantic,
 * title-first stacked list rather than a horizontally scrolled table. This
 * file runs under both mobile engines by virtue of its `.mobile.spec.ts`
 * suffix; desktop-table retention remains in `list-wrapping.spec.ts`.
 */

const SCREENSHOT_TITLE = "Attachments: drawer thumbnail strip + modal viewer";

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

async function openList(page: Page): Promise<void> {
  await page.locator('#view-toggle button[data-view="list"]').click();
  await expect(page.locator("#list-view")).toBeVisible();
  await expect(page.locator("#mobile-list")).toBeVisible();
  await expect(page.locator("#list-desktop")).toBeHidden();
}

async function lineCount(page: Page, selector: string): Promise<number> {
  return page.locator(selector).evaluate((element) => {
    const range = document.createRange();
    range.selectNodeContents(element);
    const tops = Array.from(range.getClientRects())
      .filter((rect) => rect.width > 0 && rect.height > 0)
      .map((rect) => Math.round(rect.top * 10) / 10);
    return new Set(tops).size;
  });
}

test("mobile rows are semantic, title-first, complete, and horizontally contained", async ({
  page,
}) => {
  await page.setViewportSize({ width: 320, height: 568 });
  const id = await createStory(page, SCREENSHOT_TITLE);
  const unbrokenTitle = `SH-614-${"uninterrupted".repeat(18)}`;
  const unbrokenId = await createStory(page, unbrokenTitle);
  await openList(page);

  const list = page.getByRole("list", { name: "Stories" });
  await expect(list).toBeVisible();
  const item = list.locator(`li[data-id="${id}"]`);
  await expect(item).toHaveRole("listitem");

  const title = item.getByRole("button", { name: SCREENSHOT_TITLE, exact: true });
  await expect(title).toBeVisible();
  await expect(title).toHaveCSS("font-size", "16px");
  await expect(title).toHaveCSS("line-height", "22.4px");
  expect(
    await lineCount(page, `.mobile-story-row[data-id="${id}"] .mobile-story-title`),
  ).toBeLessThanOrEqual(4);

  await expect(item.locator(".mobile-story-id")).toHaveText(id);
  await expect(item.locator(".type-badge")).toBeVisible();
  await expect(item.locator(".state-pill")).toBeVisible();
  await expect(item.locator(".mobile-story-priority")).toContainText("medium");
  await expect(item.getByRole("button", { name: `Actions for ${id}` })).toBeVisible();
  await expect(item.getByRole("button", { name: `Details for ${id}` })).toHaveAttribute(
    "aria-expanded",
    "false",
  );

  const longItem = list.locator(`li[data-id="${unbrokenId}"]`);
  await expect(longItem.locator(".mobile-story-title")).toHaveText(unbrokenTitle);
  const containment = await page.evaluate((storyId) => {
    const node = document.querySelector<HTMLElement>(
      `.mobile-story-row[data-id="${storyId}"] .mobile-story-title`,
    )!;
    return {
      documentWidth: document.documentElement.scrollWidth,
      viewportWidth: document.documentElement.clientWidth,
      title: { scrollWidth: node.scrollWidth, clientWidth: node.clientWidth },
    };
  }, unbrokenId);
  expect(containment.documentWidth).toBeLessThanOrEqual(containment.viewportWidth);
  expect(containment.title.scrollWidth).toBeLessThanOrEqual(containment.title.clientWidth);
});

test("Details exposes secondary metadata and stays open only within the current project visit", async ({
  page,
}) => {
  await openList(page);
  const item = page.locator("#mobile-list-body > li").first();
  const id = (await item.getAttribute("data-id"))!;
  const detailsButton = item.getByRole("button", { name: `Details for ${id}` });
  await detailsButton.click();
  await expect(detailsButton).toHaveAttribute("aria-expanded", "true");

  const details = item.locator(".mobile-story-details");
  await expect(details).toBeVisible();
  for (const label of [
    "Order",
    "Labels",
    "Assignee",
    "Updated",
    "Type",
    "State details",
    "Blocked",
    "Full Auto",
  ]) {
    await expect(details.locator("dt", { hasText: label })).toHaveCount(1);
  }

  await page.setViewportSize({ width: 769, height: 844 });
  await expect(page.locator("#list-desktop")).toBeVisible();
  await expect(page.locator("#mobile-list")).toBeHidden();
  await page.setViewportSize({ width: 768, height: 844 });
  await expect(
    page.locator(`.mobile-story-row[data-id="${id}"] .mobile-story-details`),
  ).toBeVisible();

  await page.locator("#projsel-btn").click();
  await page.locator("#projsel-menu .projsel-item", { hasText: "Beta Project" }).click();
  await expect(page.locator("#projsel-btn")).toContainText("Beta Project");
  await page.locator("#projsel-btn").click();
  await page.locator("#projsel-menu .projsel-item", { hasText: "Alpha Project" }).click();
  await expect(page.locator("#projsel-btn")).toContainText("Alpha Project");
  await expect(page.locator(`.mobile-story-row[data-id="${id}"] .mobile-story-details`)).toBeHidden();

  await page.locator(`.mobile-story-row[data-id="${id}"] .mobile-story-details-btn`).click();
  await page.reload();
  await expect(page.locator(`.mobile-story-row[data-id="${id}"] .mobile-story-details`)).toBeHidden();
});

test("mobile sorting shares desktop order and focus transfers across the breakpoint", async ({
  page,
}) => {
  await openList(page);
  const columns = ["id", "order", "title", "state", "priority", "assignee", "updated"];
  for (const column of columns) {
    for (const direction of ["1", "-1"]) {
      await page.locator("#mobile-sort-column").selectOption(column);
      await page.locator("#mobile-sort-direction").selectOption(direction);
      const mobileOrder = await page.locator("#mobile-list-body > li").evaluateAll((items) =>
        items.map((item) => (item as HTMLElement).dataset.id),
      );
      await page.setViewportSize({ width: 769, height: 844 });
      await expect(page.locator("#list-desktop")).toBeVisible();
      const desktopOrder = await page.locator("#list-body > tr").evaluateAll((rows) =>
        rows.map((row) => (row as HTMLElement).dataset.id),
      );
      expect(desktopOrder, `${column}/${direction} diverged between presentations`).toEqual(
        mobileOrder,
      );
      await page.setViewportSize({ width: 390, height: 844 });
      await expect(page.locator("#mobile-list")).toBeVisible();
    }
  }

  const titles = page.locator(".mobile-story-title");
  await titles.first().focus();
  await page.keyboard.press("End");
  await expect(titles.last()).toBeFocused();
  await page.keyboard.press("Home");
  await expect(titles.first()).toBeFocused();
  await page.keyboard.press("ArrowDown");
  const focusedId = await page.evaluate(() => (document.activeElement as HTMLElement).dataset.id);
  expect(focusedId).toBeTruthy();

  await page.setViewportSize({ width: 769, height: 844 });
  await expect(page.locator(`#list-body > tr[data-id="${focusedId}"]`)).toBeFocused();
  await page.setViewportSize({ width: 768, height: 844 });
  await expect(page.locator(`.mobile-story-title[data-id="${focusedId}"]`)).toBeFocused();

  // This is the event both Shift+F10 and the Menu key raise on a focused
  // element. Mobile emulation does not synthesize it from Playwright's
  // keyboard API, so drive the browser event directly, matching the
  // established story-context-menu keyboard regression.
  await page.locator(`.mobile-story-title[data-id="${focusedId}"]`).evaluate((node) => {
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
  await expect(page.locator('.ctxmenu[aria-label="Story actions"]')).toBeVisible();
  await page.keyboard.press("Escape");
  const keyboardTitle = page.locator(`.mobile-story-title[data-id="${focusedId}"]`);
  await keyboardTitle.evaluate((node) => {
    (node as HTMLElement).dataset.keyboardClicks = "0";
    node.addEventListener("click", () => {
      const element = node as HTMLElement;
      element.dataset.keyboardClicks = String(Number(element.dataset.keyboardClicks) + 1);
    });
  });
  await keyboardTitle.press(" ");
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(keyboardTitle).toHaveAttribute("data-keyboard-clicks", "1");
  await page.keyboard.press("Escape");
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  await keyboardTitle.press("Enter");
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(keyboardTitle).toHaveAttribute("data-keyboard-clicks", "2");
});

test("removing the focused story moves the mobile roving stop to a survivor", async ({
  page,
  request,
}) => {
  const title = "SH-614 focused mobile story removed externally";
  const id = await createStory(page, title);
  await openList(page);

  const removed = page.locator(`.mobile-story-title[data-id="${id}"]`);
  await removed.focus();
  await expect(removed).toBeFocused();

  const slug = await projectSlug(request, "Alpha Project");
  const response = await request.delete(
    `/api/repos/${encodeURIComponent(slug)}/story/${encodeURIComponent(id)}`,
    {
      headers: {
        "X-Storyhook": "1",
        "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
      },
      data: { force: true },
    },
  );
  expect(response.ok()).toBe(true);

  await expect(removed).not.toBeVisible();
  const survivingStop = page.locator('.mobile-story-title[tabindex="0"]');
  await expect(survivingStop).toHaveCount(1);
  await expect(survivingStop).toBeFocused();
});
