import type { APIRequestContext, Page } from "@playwright/test";
import {
  test, expect, openProject, openFilters, seedToken, projectSlug,
  requiredEnv, cleanUpCreatedStories, onAFrozenClock, awaitSettled,
} from "./support";

/** SH-729: assert geometry before a locator action can scroll the page back.
 * A nowrap toolbar used to overflow at intermediate desktop widths. Focus
 * could then move the hidden root scroller, stranding the board's left edge. */
cleanUpCreatedStories("Alpha Project");

const headers = {
  "X-Storyhook": "1",
  "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
  "Content-Type": "application/json",
};

/** Create test-owned data through the daemon; a state change is a separate API. */
async function createCard(page: Page, request: APIRequestContext, title: string, state: string) {
  const slug = await projectSlug(request, "Alpha Project");
  const response = await request.post(`/api/repos/${slug}/story`, { headers, data: { title } });
  expect(response.ok(), await response.text()).toBe(true);
  const card = page.locator(".card", { hasText: title });
  await expect(card).toHaveCount(1);
  const id = await card.getAttribute("data-id");
  expect(id).toBeTruthy();
  if (state !== "todo") {
    const moved = await request.post(`/api/repos/${slug}/story/${id}/move`, {
      headers, data: { state },
    });
    expect(moved.ok(), await moved.text()).toBe(true);
    await expect(page.locator(`.column[data-state="${state}"] .card[data-id="${id}"]`)).toHaveCount(1);
  }
  await awaitSettled(page.locator("#board-view"));
  return { card, id, slug };
}

/** Read all potential scroll owners without bringing a locator into view. */
async function geometry(page: Page) {
  return page.evaluate(() => {
    const ids = ["app", "dashboard-header", "repo-workspace", "workspace-content", "board-view"];
    return {
      root: { x: window.scrollX, width: document.documentElement.scrollWidth, viewport: innerWidth },
      boxes: ids.map(id => {
        const n = document.getElementById(id)!;
        const rect = n.getBoundingClientRect();
        return { id, left: rect.left, right: rect.right, width: n.clientWidth, extent: n.scrollWidth, scroll: n.scrollLeft };
      }),
    };
  });
}

/** The shell has no horizontal offset; only the board owns horizontal reading. */
async function expectShellAtOrigin(page: Page) {
  const actual = await geometry(page);
  const diagnostic = JSON.stringify(actual);
  expect(actual.root.x, diagnostic).toBe(0);
  expect(actual.root.width, diagnostic).toBeLessThanOrEqual(actual.root.viewport);
  for (const box of actual.boxes) {
    expect(Math.abs(box.left), diagnostic).toBeLessThanOrEqual(1);
    if (box.id !== "board-view") expect(box.scroll, diagnostic).toBe(0);
  }
}

/** Drive two real safety polls, including their response delivery. */
async function idleThroughPolls(page: Page) {
  await onAFrozenClock(page, async () => {
    for (let i = 0; i < 2; i++) {
      const next = page.waitForResponse(response => response.url().endsWith("/data") && response.ok());
      await page.clock.runFor(25_000);
      await (await next).finished();
      await page.clock.runFor(500);
    }
  });
}

for (const { width, motion } of [
  { width: 1280, motion: "no-preference" },
  { width: 1024, motion: "no-preference" },
  { width: 769, motion: "no-preference" },
  { width: 768, motion: "no-preference" },
  { width: 390, motion: "no-preference" },
  { width: 769, motion: "reduce" },
] as const) {
  test(`toolbar and board remain reachable after refresh at ${width}px (${motion})`, async ({ page, request }) => {
    await page.setViewportSize({ width, height: 900 });
    await page.emulateMedia({ reducedMotion: motion });
    await page.clock.install();
    await seedToken(page);
    await page.goto("/");
    await openProject(page, "Alpha Project");
    await createCard(page, request, "SH-729 closed scroll target", "done");
    const { card, id, slug } = await createCard(page, request, "SH-729 live scroll target", "review");
    const board = page.locator("#board-view");

    await card.focus();
    await board.evaluate(n => { n.scrollLeft = n.scrollWidth; });
    const right = await board.evaluate(n => n.scrollLeft);
    expect(right).toBeGreaterThan(0);
    // Focus is intentional setup. Never click or focus anything after the
    // failure trigger until the geometry assertions have observed it.
    await page.locator(width > 768 ? "#drafts-btn" : "#search-input").focus();
    await idleThroughPolls(page);
    const update = await request.post(`/api/repos/${slug}/story/${id}/priority`, {
      headers, data: { priority: "critical" },
    });
    expect(update.ok(), await update.text()).toBe(true);
    await expect(card).toHaveAttribute("style", /p-critical/);
    expect(await board.evaluate(n => n.scrollLeft)).toBe(right);
    await board.evaluate(n => { n.scrollLeft = 0; });
    await expectShellAtOrigin(page);
    const first = await board.locator(".column").first().boundingBox();
    expect(first!.x).toBeGreaterThanOrEqual(0);

    // Reflow must retain the controls, not merely clip the overflow away.
    for (const id of ["projsel-btn", "search-input", "new-story-btn", width > 768 ? "drafts-btn" : "more-btn"]) {
      const box = await page.locator(`#${id}`).boundingBox();
      expect(box, id).not.toBeNull();
      expect(box!.x, id).toBeGreaterThanOrEqual(0);
      expect(box!.x + box!.width, id).toBeLessThanOrEqual(width + 1);
    }

    await card.click();
    await expect(page.locator("#drawer")).toHaveClass(/open/);
    await awaitSettled(page.locator("#drawer"));
    await idleThroughPolls(page);
    await expectShellAtOrigin(page);
    await page.locator("#drawer-close").click();
    await awaitSettled(page.locator("#drawer"));
    await board.evaluate(n => { n.scrollLeft = 0; });
    await expectShellAtOrigin(page);

    await openFilters(page);
    await page.locator("#toggle-hide-empty-columns").check();
    await expect(board.locator('.column[data-state="blocked"]')).toHaveCount(0);
    await board.evaluate(n => { n.scrollLeft = 0; });
    await expectShellAtOrigin(page);
  });
}

for (const motion of ["no-preference", "reduce"] as const) {
  test(`a live reorder preserves the reading position of a focused story (${motion})`, async ({ page, request }) => {
    await page.emulateMedia({ reducedMotion: motion });
    await seedToken(page);
    await page.goto("/");
    await openProject(page, "Alpha Project");
    const { card, id, slug } = await createCard(page, request, "SH-729 focused reorder", "todo");
    const board = page.locator("#board-view");
    await card.focus();
    await board.evaluate(n => { n.scrollLeft = n.scrollWidth; });
    const right = await board.evaluate(n => n.scrollLeft);
    expect(right).toBeGreaterThan(0);
    const changed = await request.post(`/api/repos/${slug}/story/${id}/priority`, {
      headers, data: { priority: "critical" },
    });
    expect(changed.ok(), await changed.text()).toBe(true);
    await expect(board.locator('.column[data-state="todo"] .card').first()).toHaveAttribute("data-id", id!);
    await expect(card).toBeFocused();
    await awaitSettled(board);
    expect(await board.evaluate(n => n.scrollLeft)).toBe(right);
    await expectShellAtOrigin(page);

    // Deliberate keyboard navigation must still reveal the new focus target.
    await page.keyboard.press("ArrowDown");
    await expect(card).not.toBeFocused();
    expect(await board.evaluate(n => n.scrollLeft)).toBeLessThan(right);

    // Losing the story is a focus transfer, not restoration of the same node.
    await card.focus();
    await board.evaluate(n => { n.scrollLeft = n.scrollWidth; });
    const removed = await request.delete(`/api/repos/${slug}/story/${id}`, {
      headers, data: { force: true },
    });
    expect(removed.ok(), await removed.text()).toBe(true);
    await expect(card).toHaveCount(0);
    await expect(board.locator('.card[tabindex="0"]')).toBeFocused();
    expect(await board.evaluate(n => n.scrollLeft)).toBeLessThan(right);
    await expectShellAtOrigin(page);
  });
}

// The List uses the same restoration policy. Its vertical scroller catches
// accidental fixes that preserve only the board's horizontal offset.
test("a live list reorder preserves the vertical reading position", async ({ page, request }) => {
  await page.setViewportSize({ width: 1280, height: 240 });
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
  const { id, slug } = await createCard(page, request, "SH-729 list reorder", "todo");
  await createCard(page, request, "SH-729 list neighbor", "todo");
  await page.locator('#view-toggle button[data-view="list"]').click();
  await page.locator('th[data-col="priority"]').click();
  const row = page.locator(`#list-body tr[data-id="${id}"]`);
  const list = page.locator("#list-view");
  await row.focus();
  await list.evaluate(n => { n.scrollTop = n.scrollHeight; });
  const bottom = await list.evaluate(n => n.scrollTop);
  expect(bottom).toBeGreaterThan(0);
  const changed = await request.post(`/api/repos/${slug}/story/${id}/priority`, {
    headers, data: { priority: "critical" },
  });
  expect(changed.ok(), await changed.text()).toBe(true);
  await expect(page.locator("#list-body tr").first()).toHaveAttribute("data-id", id!);
  await expect(row).toBeFocused();
  expect(await list.evaluate(n => n.scrollHeight - n.clientHeight)).toBeGreaterThan(0);
  expect(await list.evaluate(n => n.scrollTop)).toBe(bottom);
  await expectShellAtOrigin(page);
});
