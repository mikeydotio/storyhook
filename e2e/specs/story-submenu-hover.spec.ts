import { test, expect, cleanUpCreatedStories, openProject, seedToken } from "./support";
import type { Page } from "@playwright/test";

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await page.locator("#new-story-btn").click();
  await page.locator("#create-title").fill("SH-715 hover fixture");
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  await expect(card(page)).toBeVisible();
});

function card(page: Page) {
  return page.locator(".card", { hasText: "SH-715 hover fixture" });
}

function menu(page: Page) {
  return page.getByRole("menu", { name: "Story actions", exact: true });
}

function parent(page: Page, name = "Set Status") {
  return menu(page).getByRole("menuitem", { name, exact: true });
}

test("primary menus require activation; hovering submenus switches without a write", async ({ page }) => {
  const sortButton = page.locator('.column[data-state="todo"] .column-sort-btn');
  await sortButton.hover();
  await expect(page.locator(".ctxmenu")).toHaveCount(0);
  await sortButton.click();
  const sortMenu = page.getByRole("menu", { name: "Sort todo", exact: true });
  await expect(sortMenu).toBeVisible();
  await sortMenu.getByRole("menuitemradio").last().hover();
  await expect(sortMenu).toBeVisible();
  await expect(page.locator(".ctxmenu-sub")).toHaveCount(0);
  await page.keyboard.press("Escape");
  const actions = card(page).locator(".card-actions-btn");
  await actions.hover();
  await expect(menu(page)).toHaveCount(0);
  await actions.click();
  const writes: string[] = [];
  page.on("request", (request) => {
    if (request.method() === "POST") writes.push(request.url());
  });
  const status = parent(page);
  const priority = parent(page, "Set Priority");
  await status.hover();
  await expect(page.getByRole("menu", { name: "Set status", exact: true })).toBeVisible();
  await expect(status).toBeFocused();
  await expect(status).toHaveAttribute("aria-expanded", "true");
  await priority.hover();
  await expect(page.locator(".ctxmenu-sub")).toHaveCount(1);
  await expect(page.getByRole("menu", { name: "Set priority", exact: true })).toBeVisible();
  await expect(status).toHaveAttribute("aria-expanded", "false");
  await expect(priority).toHaveAttribute("aria-expanded", "true");
  await parent(page, "Copy ID").hover();
  await expect(page.locator(".ctxmenu-sub")).toHaveCount(0);
  await expect(priority).toHaveAttribute("aria-expanded", "false");
  expect(writes).toEqual([]);
});

for (const side of ["right", "left"] as const) {
  test(`pointer travel into the ${side} submenu preserves its node and applies priority`, async ({ page }) => {
    if (side === "left") {
      await page.setViewportSize({ width: 1100, height: 900 });
      await page.locator('#view-toggle button[data-view="list"]').click();
      const row = page.locator("#list-body tr", { hasText: "SH-715 hover fixture" });
      await expect(row).toBeVisible();
      const box = await row.boundingBox();
      if (!box) throw new Error("story row has no bounding box");
      await row.click({ button: "right", position: { x: box.width - 12, y: box.height / 2 } });
    } else {
      await card(page).locator(".card-actions-btn").click();
    }
    const item = parent(page, "Set Priority");
    await item.hover();
    const submenu = page.getByRole("menu", { name: "Set priority", exact: true });
    await expect(submenu).toBeVisible();
    const node = await submenu.elementHandle();
    const parentBox = await item.boundingBox();
    const subBox = await submenu.boundingBox();
    if (!node || !parentBox || !subBox) throw new Error("open submenu has no geometry");
    if (side === "left") expect(subBox.x + subBox.width).toBeLessThanOrEqual(parentBox.x + 1);
    else expect(subBox.x).toBeGreaterThanOrEqual(parentBox.x + parentBox.width - 1);
    // Real intermediate mouse positions expose parent-leave dismissal bugs.
    const targetX = side === "left" ? subBox.x + subBox.width - 10 : subBox.x + 10;
    await page.mouse.move(targetX, subBox.y + 15, { steps: 12 });
    await expect(submenu).toBeVisible();
    await item.hover();
    await item.click();
    expect(await node.evaluate((element) => element.isConnected)).toBe(true);
    await expect(submenu.locator(".ctxmenu-item").first()).toBeFocused();
    await submenu.getByRole("menuitemradio", { name: "critical" }).click();
    await expect(page.locator(".ctxmenu")).toHaveCount(0);
    const story = side === "left"
      ? page.locator("#list-body tr", { hasText: "SH-715 hover fixture" })
      : card(page);
    await story.click({ button: "right" });
    await parent(page, "Set Priority").hover();
    await expect(page.getByRole("menuitemradio", { name: "critical" })).toHaveAttribute("aria-checked", "true");
  });
}

for (const key of ["Enter", "Space", "ArrowRight"]) {
  test(`hover followed by ${key} enters submenu; return restores keyboard position`, async ({ page }) => {
    await card(page).click({ button: "right" });
    const item = parent(page);
    await item.hover();
    await expect(page.locator(".ctxmenu-sub")).toBeVisible();
    await page.keyboard.press(key);
    await expect(page.locator(".ctxmenu-sub .ctxmenu-item").first()).toBeFocused();
    await page.keyboard.press("ArrowLeft");
    await expect(page.locator(".ctxmenu-sub")).toHaveCount(0);
    await expect(item).toBeFocused();
    await page.keyboard.press("ArrowDown");
    await expect(parent(page, "Set Priority")).toBeFocused();
    await page.keyboard.press("ArrowRight");
    await expect(page.getByRole("menu", { name: "Set priority", exact: true })).toBeVisible();
  });
}

test("hovered submenu supports dismissal, keyboard navigation and clean reopening", async ({ page }) => {
  const actions = card(page).locator(".card-actions-btn");
  for (const key of ["Escape", "ArrowLeft"]) {
    await actions.click();
    await parent(page).hover();
    await expect(page.locator(".ctxmenu-sub")).toBeVisible();
    await page.keyboard.press(key);
    await expect(page.locator(".ctxmenu-sub")).toHaveCount(0);
    await expect(parent(page)).toBeFocused();
    await page.keyboard.press("Escape");
    await expect(menu(page)).toHaveCount(0);
  }
  for (const key of ["ArrowDown", "ArrowUp", "Home", "End"]) {
    await actions.click();
    await parent(page).hover();
    await expect(page.locator(".ctxmenu-sub")).toBeVisible();
    await page.keyboard.press(key);
    await expect(page.locator(".ctxmenu-sub")).toHaveCount(0);
    await page.keyboard.press("Escape");
  }
  for (const dismissal of ["Tab", "outside", "popover"]) {
    await actions.click();
    await parent(page).hover();
    await expect(page.locator(".ctxmenu-sub")).toBeVisible();
    if (dismissal === "Tab") await page.keyboard.press("Tab");
    else if (dismissal === "outside") await page.locator("#board-view").click({ position: { x: 2, y: 2 } });
    else await page.locator("#new-story-btn").click();
    await expect(page.locator(".ctxmenu")).toHaveCount(0);
    if (dismissal === "popover") await page.keyboard.press("Escape");
  }
});

test("hover status selection persists without a parent click", async ({ page }) => {
  await card(page).click({ button: "right" });
  await parent(page).hover();
  await page.getByRole("menu", { name: "Set status", exact: true })
    .getByRole("menuitem", { name: "in-progress", exact: true }).click();
  await expect(page.locator(".ctxmenu")).toHaveCount(0);
  await expect(page.locator('.column[data-state="in-progress"] .card', { hasText: "SH-715 hover fixture" })).toBeVisible();
});
