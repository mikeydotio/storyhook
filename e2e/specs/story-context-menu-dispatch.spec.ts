import { test, expect } from "@playwright/test";
import { cleanUpCreatedStories, deleteStory, seedToken } from "./support";

/**
 * Exercises SH-197's context menu Dispatch/Dispatch Auto group, gated
 * identically to the drawer footer's own Dispatch buttons (SH-50/SH-208,
 * `docs/spec/dashboard-dispatch.md`'s As-built section for this story names
 * the shared expression). Every dispatch request here is stubbed via
 * `page.route` -- that the endpoint really runs `story.sh` end to end is
 * already proven by `dispatch.spec.ts` against Alpha's real story; stubbing
 * here means this file never claims that story out from under it and never
 * pays for a real worktree.
 *
 * Fixtures, from `scripts/run-e2e.sh`: "Alpha Project" (checkout, open
 * stories) and "Gamma Archive" (`--no-attach`: no checkout, one story
 * "Archived idea") -- the same two AC1 (`dispatch.spec.ts`) already relies
 * on for "has a checkout" vs. "doesn't".
 */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
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

/** A stubbed dispatch envelope (`src/api/dispatch.rs`'s `DispatchEnvelope`)
 * already in its finished state -- the client always follows a POST with a
 * GET poll regardless of what state the POST itself reports, so returning
 * "ok" immediately from both is enough to short-circuit the real 5s poll
 * interval without needing a "running" intermediate leg. */
function stubbedDispatchBody(storyId: string, auto: boolean) {
  return JSON.stringify({
    result: "ok",
    dispatch: {
      handle: "stub-handle",
      project: "alpha",
      story: storyId,
      auto,
      state: "ok",
      started_at: "2026-01-01T00:00:00Z",
      finished_at: "2026-01-01T00:00:01Z",
      payload: { display: "Stubbed dispatch complete" },
    },
  });
}

test("Dispatch and Dispatch Auto are present for an open story with a checkout", async ({
  page,
}) => {
  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await expect(page.locator("#board-view")).toBeVisible();
  const title = "SH-197 context menu — dispatch present";
  const card = await createStory(page, title);

  await card.click({ button: "right" });
  const menu = page.locator(".ctxmenu");
  await expect(menu.locator(".ctxmenu-item", { hasText: /^Dispatch$/ })).toBeVisible();
  await expect(
    menu.locator(".ctxmenu-item", { hasText: "Dispatch Auto" }),
  ).toBeVisible();

  await page.keyboard.press("Escape");
  await deleteStory(page, title);
});

test("Dispatch and Dispatch Auto are absent for a story with no checkout, and no stray separator is left behind (AC1)", async ({
  page,
}) => {
  await page.locator(".repo-card-name", { hasText: "Gamma Archive" }).click();
  await expect(page.locator("#board-view")).toBeVisible();

  const card = page.locator(".card", { hasText: "Archived idea" });
  await card.click({ button: "right" });
  const menu = page.locator(".ctxmenu");
  await expect(menu).toBeVisible();

  await expect(menu.locator(".ctxmenu-item", { hasText: /^Dispatch$/ })).toHaveCount(0);
  await expect(menu.locator(".ctxmenu-item", { hasText: "Dispatch Auto" })).toHaveCount(0);
  // Copy ID/URL/Description, Set Status and Delete survive (5, not 7) --
  // and the TWO separators either side of the now-empty dispatch group
  // collapse to exactly ONE (not zero: one still belongs between the copy
  // group and Set Status), never a stray double -- compactSeparators()'s
  // whole job. The separator before Delete is untouched either way, since
  // nothing hid Delete itself.
  await expect(menu.locator(".ctxmenu-item")).toHaveCount(5);
  await expect(menu.locator(".ctxmenu-item", { hasText: "Set Status" })).toBeVisible();
  await expect(menu.locator(".ctxmenu-item", { hasText: "Delete" })).toBeVisible();
  await expect(menu.locator(".ctxmenu-sep")).toHaveCount(2);

  await page.keyboard.press("Escape");
});

test("Dispatch issues POST .../dispatch", async ({ page }) => {
  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await expect(page.locator("#board-view")).toBeVisible();
  const title = "SH-197 context menu — dispatch stubbed";
  const card = await createStory(page, title);
  const id = await card.getAttribute("data-id");

  const requests: { url: string; method: string }[] = [];
  await page.route("**/dispatch**", async (route) => {
    const req = route.request();
    requests.push({ url: req.url(), method: req.method() });
    await route.fulfill({
      status: req.method() === "POST" ? 202 : 200,
      contentType: "application/json",
      body: stubbedDispatchBody(id!, false),
    });
  });

  await card.click({ button: "right" });
  await page.locator(".ctxmenu-item", { hasText: /^Dispatch$/ }).click();
  await expect(page.locator("#toast-stack .toast.success")).toBeVisible();

  const postReq = requests.find((r) => r.method === "POST");
  expect(postReq).toBeTruthy();
  expect(new URL(postReq!.url).pathname).toContain(`/story/${id}/dispatch`);
  expect(new URL(postReq!.url).search).toBe("");

  await deleteStory(page, title);
});

test("Dispatch Auto issues POST .../dispatch?auto=1", async ({ page }) => {
  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await expect(page.locator("#board-view")).toBeVisible();
  const title = "SH-197 context menu — dispatch auto stubbed";
  const card = await createStory(page, title);
  const id = await card.getAttribute("data-id");

  const requests: { url: string; method: string }[] = [];
  await page.route("**/dispatch**", async (route) => {
    const req = route.request();
    requests.push({ url: req.url(), method: req.method() });
    await route.fulfill({
      status: req.method() === "POST" ? 202 : 200,
      contentType: "application/json",
      body: stubbedDispatchBody(id!, true),
    });
  });

  await card.click({ button: "right" });
  await page.locator(".ctxmenu-item", { hasText: "Dispatch Auto" }).click();
  // An --auto completion doesn't toast (SH-232) -- nobody is necessarily
  // watching, so it becomes a durable #dispatch-history row instead.
  await expect(page.locator("#dispatch-history .dispatch-history-row")).toBeVisible();

  const postReq = requests.find((r) => r.method === "POST");
  expect(postReq).toBeTruthy();
  expect(new URL(postReq!.url).search).toBe("?auto=1");

  // Durable until dismissed, and fixed bottom-right -- left alone it
  // overlaps the drawer footer's own buttons, which is what deleteStory()
  // needs to click next.
  await page.locator(".dispatch-history-dismiss").click();
  await deleteStory(page, title);
});

test("both items are aria-disabled while a dispatch for this story is in flight", async ({
  page,
}) => {
  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await expect(page.locator("#board-view")).toBeVisible();
  const title = "SH-197 context menu — dispatch in flight";
  const card = await createStory(page, title);
  const id = await card.getAttribute("data-id");

  // Delayed, not immediate: the window this test needs to re-open the menu
  // and observe the disabled state before the stubbed dispatch "finishes".
  await page.route("**/dispatch**", async (route) => {
    const req = route.request();
    if (req.method() === "POST") {
      await route.fulfill({
        status: 202,
        contentType: "application/json",
        body: stubbedDispatchBody(id!, false).replace('"state":"ok"', '"state":"running"'),
      });
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 1000));
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: stubbedDispatchBody(id!, false),
    });
  });

  await card.click({ button: "right" });
  await page.locator(".ctxmenu-item", { hasText: /^Dispatch$/ }).click();

  await card.click({ button: "right" });
  const menu = page.locator(".ctxmenu");
  await expect(menu).toBeVisible();
  await expect(
    menu.locator(".ctxmenu-item", { hasText: /^Dispatch$/ }),
  ).toHaveAttribute("aria-disabled", "true");
  await expect(
    menu.locator(".ctxmenu-item", { hasText: "Dispatch Auto" }),
  ).toHaveAttribute("aria-disabled", "true");

  await page.keyboard.press("Escape");
  await expect(page.locator("#toast-stack .toast.success")).toBeVisible({
    timeout: 10_000,
  });
  await deleteStory(page, title);
});
