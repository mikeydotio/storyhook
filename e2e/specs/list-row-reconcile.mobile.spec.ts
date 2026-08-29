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
 * - an unchanged row retains its cells and focused actions button; and
 * - while SH-401 has landed new state but deferred its paint, that old button
 *   builds the actions menu from the current story rather than its render-time
 *   closure.
 *
 * The actions button is visible only under `(pointer: coarse)` (SH-235), so
 * this file deliberately runs in mobile-chromium and mobile-webkit only.
 */

type BoardSnapshot = {
  stories: Array<{
    story: { id: string; description: string; priority: string };
  }>;
};

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto(`/?boardFetchTimeoutMs=${heldReadDeadlineMs()}`);
  await openProject(page, "Alpha Project");
});

cleanUpCreatedStories("Alpha Project");

/** Applies a real external PATCH, then returns the exact `/data` reply that
 * carries it. The page sees the mutation only when the caller delivers the
 * held reply, so DOM identity and press timing are deterministic rather than
 * dependent on SSE/network scheduling. */
async function holdStoryPatch(
  page: Page,
  request: APIRequestContext,
  id: string,
  patch: { description?: string; priority?: string },
) {
  const held = await holdFetch<BoardSnapshot>(
    page,
    (url) => url.pathname.endsWith("/data"),
    (body) =>
      body.stories.some(({ story }) => {
        if (story.id !== id) return false;
        if (patch.description !== undefined && story.description !== patch.description)
          return false;
        if (patch.priority !== undefined && story.priority !== patch.priority) return false;
        return true;
      }),
  );

  const slug = await projectSlug(request, "Alpha Project");
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
  const row = page.locator(`#list-body tr[data-id="${id}"]`);
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
  const { actions } = await openListRow(page, id);

  await actions.evaluate((node) => {
    (node as HTMLElement & { __sh425Original?: boolean }).__sh425Original = true;
  });
  await actions.focus();
  await expect(actions).toBeFocused();

  // Description is deliberately absent from list-row output. The reply is a
  // real state change and a real render request, but a no-op for this row.
  const held = await holdStoryPatch(page, request, id, {
    description: "SH-425 changed only the non-rendered description",
  });
  await held.deliver();

  expect(
    await actions.evaluate(
      (node) => !!(node as HTMLElement & { __sh425Original?: boolean }).__sh425Original,
    ),
    "the live actions button must be the same node, not an identical replacement",
  ).toBe(true);
  await expect(actions).toBeFocused();

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
  await expect(row.locator("td").nth(4)).toContainText("medium");
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
  await expect(row.locator("td").nth(4)).toContainText("high");

  await page.keyboard.press("Escape");
  await page.keyboard.press("Escape");
  await expect(page.locator(".ctxmenu")).toHaveCount(0);
  await page.locator('#view-toggle button[data-view="board"]').click();
  await deleteStory(page, title);
});
