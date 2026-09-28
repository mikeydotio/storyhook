import {
  test,
  expect,
  cleanUpCreatedStories,
  onAFrozenClock,
  openProject,
  seedToken,
} from "./support";
import type { Page } from "@playwright/test";

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  // Time still flows; only the pointer-travel tests freeze it (SH-811).
  await page.clock.install();
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

function listRow(page: Page) {
  return page.locator("#list-body tr", { hasText: "SH-715 hover fixture" });
}

function menu(page: Page) {
  return page.getByRole("menu", { name: "Story actions", exact: true });
}

function parent(page: Page, name = "Set Status") {
  return menu(page).getByRole("menuitem", { name, exact: true });
}

function submenuNamed(page: Page, name: "Set priority" | "Set status") {
  return page.getByRole("menu", { name, exact: true });
}

/**
 * The page height of every pointer-travel test (SH-811). `.ctxmenu` is capped
 * at `min(20rem, 100dvh - 16px)` = 320 px, so at this height any anchor below
 * y = 72 -- every card and list row -- leaves no room under the menu:
 * `placeMenu` pins it to the viewport bottom and lifts a submenu that cannot
 * fit below its parent. Travel into that submenu then crosses sibling rows
 * whatever order the board's cards are in. The spec used to inherit its
 * geometry from the specs that ran before it, and was green only when
 * dispatch.spec.ts had moved a card out of todo.
 */
const TRAVEL_VIEWPORT_HEIGHT = 400;

/** `placeMenu`'s margin from each viewport edge. */
const MENU_MARGIN_PX = 8;

/** Steps per travel. `page.mouse.move` interpolates linearly from the last pointer position. */
const TRAVEL_STEPS = 12;

/**
 * The dashboard's `STORY_SUBMENU_AIM_REST_MS`, restated rather than read: the
 * boundary pair in the rest test fails if either copy changes alone.
 */
const SPEC_REST_MS = 300;

/** Longer than any rest window: time alone must not take a submenu the pointer reached. */
const SETTLE_MS = 1000;

/** A slow hand's pause between two pointer samples, below the rest window. */
const SLOW_STEP_MS = 250;

/** How far inside a row a travel point must be to count as crossing it. */
const CROSSING_INSET_PX = 2;

type Side = "right" | "left";
type Target = "first" | "last";
interface Point { x: number; y: number }
interface Crossing { step: number; row: string }

/**
 * Opens the fixture's story menu at the travel height -- from the card's
 * actions button for a submenu that opens right, from the list row near the
 * viewport's right edge for one that flips left -- and asserts the menu is
 * pinned to the viewport bottom, the precondition every travel test needs.
 */
async function openTravelMenu(page: Page, side: Side): Promise<void> {
  if (side === "left") {
    await page.setViewportSize({ width: 1100, height: TRAVEL_VIEWPORT_HEIGHT });
    await page.locator('#view-toggle button[data-view="list"]').click();
    await expect(listRow(page)).toBeVisible();
    const box = await listRow(page).boundingBox();
    if (!box) throw new Error("story row has no bounding box");
    await listRow(page).click({ button: "right", position: { x: box.width - 12, y: box.height / 2 } });
  } else {
    await page.setViewportSize({ width: 1280, height: TRAVEL_VIEWPORT_HEIGHT });
    await card(page).locator(".card-actions-btn").click();
  }
  const box = await menu(page).boundingBox();
  if (!box) throw new Error("story menu has no bounding box");
  expect(
    Math.abs(box.y + box.height - (TRAVEL_VIEWPORT_HEIGHT - MENU_MARGIN_PX)),
    "the story menu must be pinned to the viewport bottom, or the submenu is not lifted",
  ).toBeLessThanOrEqual(1);
}

/**
 * Hovers `name` at the end of its row away from the submenu (the label end
 * for a right submenu), so the travel is as long as a person's, and returns
 * where the pointer now is.
 */
async function hoverParentFarEnd(page: Page, side: Side, name: string): Promise<Point> {
  const row = parent(page, name);
  const box = await row.boundingBox();
  if (!box) throw new Error(`${name} has no bounding box`);
  const x = side === "right" ? 12 : box.width - 12;
  await row.hover({ position: { x, y: box.height / 2 } });
  return { x: box.x + x, y: box.y + box.height / 2 };
}

/**
 * A point in the open submenu's first or last row, 10 px in from its near
 * edge, in the row's outer quarter -- above the parent for the first row of
 * a lifted submenu, below it for the last.
 */
async function submenuTarget(page: Page, name: "Set priority" | "Set status", side: Side, target: Target): Promise<Point> {
  const sub = submenuNamed(page, name);
  const subBox = await sub.boundingBox();
  const rows = sub.locator(".ctxmenu-item");
  const rowBox = await (target === "first" ? rows.first() : rows.last()).boundingBox();
  if (!subBox || !rowBox) throw new Error(`${name} submenu has no geometry`);
  return {
    x: side === "right" ? subBox.x + 10 : subBox.x + subBox.width - 10,
    y: rowBox.y + rowBox.height * (target === "first" ? 0.25 : 0.75),
  };
}

/** The points `page.mouse.move(to, { steps })` visits from `from`, in order. */
function travelPoints(from: Point, to: Point, steps = TRAVEL_STEPS): Point[] {
  return Array.from({ length: steps }, (_, i) => ({
    x: from.x + ((to.x - from.x) * (i + 1)) / steps,
    y: from.y + ((to.y - from.y) * (i + 1)) / steps,
  }));
}

/**
 * The first travel point that lies inside a story-menu row other than
 * `parentName` -- by `CROSSING_INSET_PX` on every side, and confirmed by
 * hit-testing -- or null. That is the point where a submenu without pointer
 * aim switches or closes. A travel that crosses no sibling proves nothing,
 * so every travel test asserts one exists before it moves the pointer.
 */
async function firstSiblingCrossing(page: Page, points: Point[], parentName: string): Promise<Crossing | null> {
  return page.evaluate(([pts, parentLabel, inset]) => {
    const main = document.querySelector('.ctxmenu[aria-label="Story actions"]');
    if (!main) throw new Error("no story menu");
    const label = (row: Element) => (row.childNodes[0]?.textContent ?? "").trim();
    const rows = Array.from(main.querySelectorAll(".ctxmenu-item")).filter((row) => label(row) !== parentLabel);
    for (let i = 0; i < pts.length; i++) {
      const { x, y } = pts[i];
      const row = rows.find((candidate) => {
        const r = candidate.getBoundingClientRect();
        return x >= r.left + inset && x <= r.right - inset && y >= r.top + inset && y <= r.bottom - inset;
      });
      if (row && document.elementFromPoint(x, y)?.closest(".ctxmenu-item") === row) {
        return { step: i + 1, row: label(row) };
      }
    }
    return null;
  }, [points, parentName, CROSSING_INSET_PX] as const);
}

/**
 * Opens the priority submenu by hover from the far end of "Set Priority",
 * plans a travel to `target`, and asserts the plan crosses a sibling row.
 * Runs inside a frozen clock, so the caller decides when time passes.
 */
async function planPriorityTravel(page: Page, side: Side, target: Target) {
  const item = parent(page, "Set Priority");
  const from = await hoverParentFarEnd(page, side, "Set Priority");
  const submenu = submenuNamed(page, "Set priority");
  await expect(submenu).toBeVisible();
  const parentBox = await item.boundingBox();
  const subBox = await submenu.boundingBox();
  if (!parentBox || !subBox) throw new Error("open submenu has no geometry");
  if (side === "left") expect(subBox.x + subBox.width).toBeLessThanOrEqual(parentBox.x + 1);
  else expect(subBox.x).toBeGreaterThanOrEqual(parentBox.x + parentBox.width - 1);
  if (target === "first") expect(subBox.y, "the submenu must be lifted above its parent").toBeLessThan(parentBox.y);
  const points = travelPoints(from, await submenuTarget(page, "Set priority", side, target));
  const crossing = await firstSiblingCrossing(page, points, "Set Priority");
  expect(crossing, "the travel must cross a sibling row, or it tests nothing").not.toBeNull();
  const node = await submenu.elementHandle();
  if (!node) throw new Error("open submenu has no node");
  return { item, submenu, node, points, crossing: crossing as Crossing };
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
  for (const target of ["first", "last"] as const) {
    test(`pointer travel across siblings to the ${target} priority keeps the ${side} submenu and applies priority`, async ({ page }) => {
      await openTravelMenu(page, side);
      let plan!: Awaited<ReturnType<typeof planPriorityTravel>>;
      await onAFrozenClock(page, async () => {
        plan = await planPriorityTravel(page, side, target);
        const last = plan.points[plan.points.length - 1];
        await page.mouse.move(last.x, last.y, { steps: TRAVEL_STEPS });
        await expect(plan.submenu).toBeVisible();
        await page.clock.runFor(SETTLE_MS);
        await expect(plan.submenu).toBeVisible();
        expect(await plan.node.evaluate((element) => element.isConnected)).toBe(true);
      });
      const { item, submenu, node } = plan;
      await item.hover();
      await item.click();
      expect(await node.evaluate((element) => element.isConnected)).toBe(true);
      await expect(submenu.locator(".ctxmenu-item").first()).toBeFocused();
      await submenu.getByRole("menuitemradio", { name: "critical" }).click();
      await expect(page.locator(".ctxmenu")).toHaveCount(0);
      const story = side === "left" ? listRow(page) : card(page);
      await story.click({ button: "right" });
      await parent(page, "Set Priority").hover();
      await expect(page.getByRole("menuitemradio", { name: "critical" })).toHaveAttribute("aria-checked", "true");
    });
  }
}

for (const [target, sibling] of [["first", "Set Status"], ["last", "Reset…"]] as const) {
  test(`a pointer that rests on ${sibling} while aiming gets it after the rest window`, async ({ page }) => {
    await openTravelMenu(page, "right");
    await onAFrozenClock(page, async () => {
      const { submenu, points, crossing } = await planPriorityTravel(page, "right", target);
      expect(crossing.row).toBe(sibling);
      const stop = points[crossing.step - 1];
      await page.mouse.move(stop.x, stop.y, { steps: crossing.step });
      await expect(submenu).toBeVisible();
      await expect(page.locator(".ctxmenu-sub")).toHaveCount(1);
      await page.clock.runFor(SPEC_REST_MS - 1);
      await expect(submenu).toBeVisible();
      await page.clock.runFor(1);
      await expect(submenu).toHaveCount(0);
      if (sibling === "Set Status") await expect(submenuNamed(page, "Set status")).toBeVisible();
      else await expect(page.locator(".ctxmenu-sub")).toHaveCount(0);
      await expect(menu(page)).toBeVisible();
      await expect(parent(page, sibling)).toBeFocused();
    });
  });
}

test("slow steady travel keeps the submenu however long it takes", async ({ page }) => {
  await openTravelMenu(page, "right");
  await onAFrozenClock(page, async () => {
    const { submenu, node, points } = await planPriorityTravel(page, "right", "first");
    for (const point of points) {
      await page.mouse.move(point.x, point.y);
      await page.clock.runFor(SLOW_STEP_MS);
    }
    await page.clock.runFor(SETTLE_MS);
    await expect(submenu).toBeVisible();
    expect(await node.evaluate((element) => element.isConnected)).toBe(true);
  });
});

test("a pointer that turns off the aim line gets the sibling at once", async ({ page }) => {
  await openTravelMenu(page, "right");
  await onAFrozenClock(page, async () => {
    const { submenu, points, crossing } = await planPriorityTravel(page, "right", "first");
    const stop = points[crossing.step - 1];
    await page.mouse.move(stop.x, stop.y, { steps: crossing.step });
    await expect(submenu).toBeVisible();
    // Back along the sibling towards its label: out of the triangle, no clock advance.
    const siblingBox = await parent(page, crossing.row).boundingBox();
    if (!siblingBox) throw new Error(`${crossing.row} has no bounding box`);
    await page.mouse.move(siblingBox.x + 12, siblingBox.y + siblingBox.height / 2);
    await expect(submenuNamed(page, "Set status")).toBeVisible();
    await expect(submenu).toHaveCount(0);
  });
});

test("a submenu opened by resting keeps its own aim for the next diagonal", async ({ page }) => {
  await openTravelMenu(page, "right");
  await onAFrozenClock(page, async () => {
    const { points, crossing } = await planPriorityTravel(page, "right", "first");
    expect(crossing.row).toBe("Set Status");
    const stop = points[crossing.step - 1];
    await page.mouse.move(stop.x, stop.y, { steps: crossing.step });
    await page.clock.runFor(SPEC_REST_MS);
    const status = submenuNamed(page, "Set status");
    await expect(status).toBeVisible();
    const node = await status.elementHandle();
    if (!node) throw new Error("Set status submenu has no node");
    const onward = travelPoints(stop, await submenuTarget(page, "Set status", "right", "last"));
    const next = await firstSiblingCrossing(page, onward, "Set Status");
    expect(next, "the onward travel must cross a sibling row, or it tests nothing").not.toBeNull();
    const end = onward[onward.length - 1];
    await page.mouse.move(end.x, end.y, { steps: TRAVEL_STEPS });
    await page.clock.runFor(SETTLE_MS);
    await expect(status).toBeVisible();
    expect(await node.evaluate((element) => element.isConnected)).toBe(true);
  });
});

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

test("keyboard scrolling keeps focus until the mouse moves again", async ({ page }) => {
  await card(page).locator(".card-actions-btn").click();
  const actions = menu(page);
  const status = parent(page);
  const last = actions.locator(".ctxmenu-item").last();
  await status.hover();
  const box = await status.boundingBox();
  if (!box) throw new Error("Set Status has no geometry");
  const pointer = { x: box.x + box.width / 2, y: box.y + box.height / 2 };
  await expect(submenuNamed(page, "Set status")).toBeVisible();
  expect(await actions.evaluate(node => node.scrollHeight > node.clientHeight),
    "End must scroll rows under the stationary pointer").toBe(true);
  // Register before the key: resolving a locator concurrently with the key
  // can miss the scroll. Observe rendering without changing menu behavior.
  await actions.evaluate(node => {
    node.addEventListener("scroll", () => requestAnimationFrame(() => {
      requestAnimationFrame(() => node.setAttribute("data-test-scroll-settled", "true"));
    }), { once: true });
  });
  await page.keyboard.press("End");
  await expect(actions).toHaveAttribute("data-test-scroll-settled", "true");
  await expect(last).toBeFocused();
  await expect(page.locator(".ctxmenu-sub")).toHaveCount(0);
  expect(await page.evaluate(point =>
    document.elementFromPoint(point.x, point.y)?.closest(".ctxmenu-item")?.textContent,
  pointer), "the scroll must put a different submenu parent under the mouse").toContain("Set Priority");

  // A move within that same row must resume hover without needing to leave it.
  await page.mouse.move(pointer.x + 1, pointer.y);
  await expect(parent(page, "Set Priority")).toBeFocused();
  await expect(submenuNamed(page, "Set priority")).toBeVisible();
});

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
