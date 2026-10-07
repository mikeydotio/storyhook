import { isBoardPage } from "./board-network";
import { test, expect } from "./support";
import type { APIRequestContext, Page } from "@playwright/test";
import {
  cleanUpCreatedStories,
  createStory,
  deleteStory,
  heldReadDeadlineMs,
  holdFetch,
  openProject,
  pressGateSwallows,
  projectSlug,
  requiredEnv,
  seedToken,
} from "./support";

/**
 * SH-425 — list rows used to clear and rebuild every cell on every render,
 * even when a reply changed nothing that row displays. SH-401 subsequently
 * protected a primary pointer gesture by deferring paint, and its desktop
 * Chromium/WebKit witness already proves a changed row cannot swallow the
 * click that opens its drawer. These coarse-pointer tests cover the two live
 * list-specific residues:
 *
 * - changed metadata preserves unchanged cells and the focused actions button; and
 * - while SH-401 has landed new state but deferred its paint, that old button
 *   builds the actions menu from the current story rather than its render-time
 *   closure.
 *
 * The actions button is visible only under `(pointer: coarse)` (SH-235), so
 * this file deliberately runs in mobile-chromium and mobile-webkit only.
 */

type BoardSnapshot = {
  stories: Array<{
    story: { id: string; priority: string };
    head_global_seq: number;
  }>;
};

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto(`/?boardFetchTimeoutMs=${heldReadDeadlineMs()}`);
  await openProject(page, "Alpha Project");
});

cleanUpCreatedStories("Alpha Project");

/** Applies a real external PATCH, then returns the exact summary-page reply that
 * carries it. The page sees the mutation only when the caller delivers the
 * held reply, so DOM identity and press timing are deterministic rather than
 * dependent on SSE/network scheduling. */
async function holdStoryPatch(
  page: Page,
  request: APIRequestContext,
  id: string,
  patch: { description?: string; priority?: string },
) {
  const slug = await projectSlug(request, "Alpha Project");
  const before = await request.get(`/api/repos/${encodeURIComponent(slug)}/story/${encodeURIComponent(id)}`, {
    headers: { "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN") },
  });
  expect(before.ok(), await before.text()).toBe(true);
  const beforeSequence: number = (await before.json()).story.head_global_seq;
  expect(beforeSequence).toEqual(expect.any(Number));
  const held = await holdFetch<BoardSnapshot>(
    page,
    (url) => isBoardPage(url, slug, null),
    (body) =>
      body.stories.some(({ story, head_global_seq }) => {
        if (story.id !== id || head_global_seq <= beforeSequence) return false;
        // Descriptions are intentionally absent from summaries. A real newer
        // write sequence proves this page carries the PATCH, even when the
        // changed field itself is only available from the detail endpoint.
        if (patch.priority !== undefined && story.priority !== patch.priority) return false;
        return true;
      }),
  );
  const changed = await request.patch(
    `/api/repos/${encodeURIComponent(slug)}/story/${encodeURIComponent(id)}`,
    {
      headers: {
        "X-Storyhook": "1",
        "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
        "Content-Type": "application/json",
      },
      data: patch,
    },
  );
  if (!changed.ok()) {
    throw new Error(
      `PATCH .../story/${id} answered ${changed.status()}: ${await changed.text()} -- ` +
        "this spec depends on the external update landing",
    );
  }
  await held.taken;
  return held;
}

async function openListRow(page: Page, id: string) {
  await page.locator('#view-toggle button[data-view="list"]').click();
  const row = page.locator(`#mobile-list-body li[data-id="${id}"]`);
  await expect(row).toBeVisible();
  const actions = row.locator(".row-actions-btn");
  await expect(actions).toBeVisible();
  return { row, actions };
}

test("an unrelated data update preserves an unchanged row action button and its focus (SH-425)", async ({
  page,
  request,
}) => {
  const title = "SH-425 list-row identity across a no-op render";
  const id = await createStory(page, title);
  const { row, actions } = await openListRow(page, id);
  await row.locator(".mobile-story-details-btn").click();
  await expect(row.locator(".mobile-story-details")).toBeVisible();

  await expect(page.locator("#list-body tr")).toHaveCount(0);
  const beforeUpdated = await row.locator(".mobile-story-details time").getAttribute("datetime");
  expect(beforeUpdated).not.toBeNull();
  // The daemon stores whole seconds. Cross that boundary so the real PATCH
  // must change Updated even when this test runs within its creation second.
  await expect.poll(() => Date.now()).toBeGreaterThan(Date.parse(beforeUpdated!) + 1000);

  await actions.evaluate((node) => {
    (node as HTMLElement & { __sh425Original?: boolean }).__sh425Original = true;
  });
  const titleControl = row.locator(".mobile-story-title");
  await titleControl.focus();
  await expect(titleControl).toHaveAttribute("tabindex", "0");
  await titleControl.evaluate((node) => {
    (node as HTMLElement & { __sh425Original?: boolean }).__sh425Original = true;
  });
  await actions.focus();
  await expect(actions).toBeFocused();

  // Description leaves the action unchanged, while Updated must refresh its
  // exact datetime/title. That separate cell must not discard the focused button.
  const held = await holdStoryPatch(page, request, id, {
    description: "SH-425 changed only the non-rendered description",
  });
  await held.deliver();
  await expect(row.locator(".mobile-story-details time")).not.toHaveAttribute("datetime", beforeUpdated!);
  expect(
    await titleControl.evaluate(
      (node) => !!(node as HTMLElement & { __sh425Original?: boolean }).__sh425Original,
    ),
    "runtime roving tabindex must not replace the unchanged title control",
  ).toBe(true);
  await expect(titleControl).toHaveAttribute("tabindex", "0");

  expect(
    await actions.evaluate(
      (node) => !!(node as HTMLElement & { __sh425Original?: boolean }).__sh425Original,
    ),
    "the live actions button must be the same node, not an identical replacement",
  ).toBe(true);
  await expect(actions).toBeFocused();
  await expect(row.locator(".mobile-story-details")).toBeVisible();

  // Preserve the desktop identity witness with that layout actually visible.
  // SH-894 deliberately evicts the hidden desktop rows on a narrow screen.
  const originalViewport = page.viewportSize()!;
  await page.setViewportSize({ width: 1280, height: originalViewport.height });
  const desktopRow = page.locator(`#list-body tr[data-id="${id}"]`);
  await expect(desktopRow).toBeVisible();
  await expect(page.locator("#mobile-list-body li")).toHaveCount(0);
  const desktopActions = desktopRow.locator(".row-actions-btn");
  await desktopActions.evaluate((node) => {
    (node as HTMLElement & { __sh425Original?: boolean }).__sh425Original = true;
  });
  const desktopUpdated = await desktopRow.locator(".col-date time").getAttribute("datetime");
  expect(desktopUpdated).not.toBeNull();
  await expect.poll(() => Date.now()).toBeGreaterThan(Date.parse(desktopUpdated!) + 1000);
  const desktopHeld = await holdStoryPatch(page, request, id, {
    description: "SH-425 visible desktop date cell refresh",
  });
  await desktopHeld.deliver();
  await expect(desktopRow.locator(".col-date time")).not.toHaveAttribute("datetime", desktopUpdated!);
  expect(
    await desktopActions.evaluate(
      (node) => !!(node as HTMLElement & { __sh425Original?: boolean }).__sh425Original,
    ),
    "the desktop date cell must not replace its unchanged actions cell",
  ).toBe(true);
  await page.setViewportSize(originalViewport);

  await page.locator('#view-toggle button[data-view="board"]').click();
  await deleteStory(page, title);
});

test("a row action pressed across a data reply opens its menu from current state (SH-425)", async ({
  page,
  request,
}) => {
  const title = "SH-425 list-row actions use current state";
  const id = await createStory(page, title); // support fixture starts at medium
  const { row, actions } = await openListRow(page, id);

  const held = await holdStoryPatch(page, request, id, { priority: "high" });
  await actions.dispatchEvent("pointerdown", {
    pointerId: 1,
    pointerType: "touch",
    isPrimary: true,
    button: 0,
    buttons: 1,
    clientX: 1,
    clientY: 1,
  });
  // SH-401 lands state.data now but defers populateListRow until the press can
  // no longer click. The old button therefore activates against newer state.
  await held.deliver();
  await expect(row.locator(".mobile-story-priority")).toContainText("medium");
  await actions.evaluate((node) => (node as HTMLElement).click());

  const menu = page.locator('.ctxmenu[aria-label="Story actions"]');
  await expect(menu).toBeVisible();
  expect(await pressGateSwallows(page)).toEqual([]);
  await menu.locator(".ctxmenu-item", { hasText: "Set Priority" }).click();

  const priorityMenu = page.locator('.ctxmenu-sub[aria-label="Set priority"]');
  await expect(priorityMenu).toBeVisible();
  await expect(priorityMenu.locator('[aria-checked="true"]')).toHaveCount(1);
  await expect(priorityMenu.locator('[aria-checked="true"]')).toContainText("high");

  await actions.dispatchEvent("pointerup", {
    pointerId: 1,
    pointerType: "touch",
    isPrimary: true,
    button: 0,
    buttons: 0,
    clientX: 1,
    clientY: 1,
  });
  await page.evaluate(
    () =>
      new Promise<void>((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
      ),
  );
  await expect(row.locator(".mobile-story-priority")).toContainText("high");

  await page.keyboard.press("Escape");
  await page.keyboard.press("Escape");
  await expect(page.locator(".ctxmenu")).toHaveCount(0);
  await page.locator('#view-toggle button[data-view="board"]').click();
  await deleteStory(page, title);
});
