import { test, expect } from "./support";
import {
  cleanUpCreatedStories,
  openProject,
  projectSlug,
  requiredEnv,
  seedToken,
  storiesInProject,
} from "./support";

/**
 * Exercises SH-779 at the dashboard: the New Story modal's "Blocked by"
 * field files a story already blocked.
 *
 * Filing a story and then relating it from the drawer is two writes, and the
 * first one wakes a Full Auto run, which can claim the story while it is
 * still ready. A NEW story must therefore carry its blockers in the creating
 * POST (`blocked_by`), never in a follow-up `/relate` — the first test pins
 * that shape on the wire as well as the result. A draft is never ready, so
 * its edit path may diff edges with separate writes before it publishes.
 *
 * This spec creates its own stories in "Alpha Project" and relies on the
 * shared cleanup, as card-blockers.spec.ts does, so the fixture's two-story
 * shape other specs assert on is left intact.
 */

cleanUpCreatedStories("Alpha Project");

const DASHBOARD_TOKEN = requiredEnv("DASHBOARD_TOKEN");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

async function createStory(
  page: import("@playwright/test").Page,
  title: string,
): Promise<string> {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  const card = page.locator('.column[data-state="todo"] .card', {
    hasText: title,
  });
  await expect(card).toBeVisible();
  return (await card.getAttribute("data-id"))!;
}

/** The `blocked-by` ids the server records for `id`. */
async function blockedByOf(
  request: import("@playwright/test").APIRequestContext,
  id: string,
): Promise<string[]> {
  const slug = await projectSlug(request, "Alpha Project");
  const resp = await request.get(
    `/api/repos/${encodeURIComponent(slug)}/story/${encodeURIComponent(id)}`,
    { headers: { "X-Storyhook-Token": DASHBOARD_TOKEN } },
  );
  expect(resp.ok()).toBe(true);
  const body = await resp.json();
  return (body.story.story.relationships ?? [])
    .filter((r: { relation: string }) => r.relation === "blocked-by")
    .map((r: { other_id: string }) => r.other_id);
}

test("Create Story sends its blockers in the creating POST and the card is born blocked", async ({
  page,
  request,
}) => {
  const blockerId = await createStory(page, "SH-779 dashboard — the blocker");
  const title = "SH-779 dashboard — filed already blocked";

  const relates: string[] = [];
  page.on("request", (req) => {
    if (req.method() === "POST" && new URL(req.url()).pathname.endsWith("/relate")) {
      relates.push(req.url());
    }
  });

  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  // A bare number is this project's id, exactly as `--blocked-by 2` is.
  await page
    .locator("#create-blocked-by")
    .fill(blockerId.replace(/^[^-]+-/, ""));
  const created = page.waitForRequest(
    (req) =>
      req.method() === "POST" && new URL(req.url()).pathname.endsWith("/story"),
  );
  await page.locator("#create-submit").click();
  const body = (await created).postDataJSON();
  expect(body.blocked_by).toEqual([blockerId]);
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);

  const card = page.locator('.column[data-state="todo"] .card', { hasText: title });
  await expect(card).toBeVisible();
  await expect(card.locator(".flag-blocked .rel-id")).toHaveText(blockerId);
  const id = (await card.getAttribute("data-id"))!;
  expect(await blockedByOf(request, id)).toEqual([blockerId]);
  expect(relates, "a new story's edges never go through a follow-up /relate").toEqual([]);
});

test("an unknown blocker keeps the modal open, names the id, and files nothing", async ({
  page,
  request,
}) => {
  const before = (await storiesInProject(request, "Alpha Project")).length;
  const title = "SH-779 dashboard — never filed";

  await page.locator("#new-story-btn").click();
  await page.locator("#create-title").fill(title);
  await page.locator("#create-blocked-by").fill("AA-9999");
  await page.locator("#create-submit").click();

  await expect(page.locator("#create-error")).toContainText("AA-9999");
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  expect((await storiesInProject(request, "Alpha Project")).length).toBe(before);

  await page.locator("#create-discard").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  await expect(page.locator(".card", { hasText: title })).toHaveCount(0);
});

test("editing a draft's blockers diffs them before Publish makes it live", async ({
  page,
  request,
}) => {
  const first = await createStory(page, "SH-779 dashboard — first blocker");
  const second = await createStory(page, "SH-779 dashboard — second blocker");
  const title = "SH-779 dashboard — a draft that waits";

  await page.locator("#new-story-btn").click();
  await page.locator("#create-title").fill(title);
  await page.locator("#create-blocked-by").fill(first);
  await page.locator("#create-save-draft").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);

  await page.locator("#drafts-btn").click();
  await page.locator("#drafts-list .drafts-row", { hasText: title }).click();
  await expect(page.locator("#create-modal-header")).toHaveText("Edit draft");
  await expect(page.locator("#create-blocked-by")).toHaveValue(first);

  // Retyping the kept blocker as a bare number must not read as a removal.
  await page
    .locator("#create-blocked-by")
    .fill(`${first.replace(/^[^-]+-/, "")}, ${second}`);
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);

  const card = page.locator(".card", { hasText: title });
  await expect(card).toBeVisible();
  const id = (await card.getAttribute("data-id"))!;
  expect((await blockedByOf(request, id)).sort()).toEqual([first, second].sort());
});
