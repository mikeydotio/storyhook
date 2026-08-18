import { test, expect } from "./support";
import {
  cleanUpCreatedStories,
  openProject,
  projectSlug,
  requiredEnv,
  seedToken,
} from "./support";

/**
 * Pins SH-245's containment rule: a spec that never reaches its own cleanup
 * must not leave a story behind in a fixture project the rest of the suite
 * reads.
 *
 * Most specs create their stories in "Alpha Project", whose exact two-story
 * shape `filter-persistence.spec.ts` and `column-visibility.spec.ts` assert
 * on by count. Every one of them deletes what it created as the *last*
 * statement of the test body, so any failure above that line strands a
 * story — and the next spec to count Alpha's cards fails too, naming
 * something that was never involved. That is how SH-245's one genuine
 * failure was reported as three.
 *
 * The first test below is the stranding, deliberately: it creates a story
 * and returns without deleting it, exactly as a test that failed mid-body
 * would. The second proves the `afterEach` swept it anyway.
 */

cleanUpCreatedStories("Alpha Project");

const STRAY = "SH-245 stray — never cleaned up by its own test";

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

test("a test can end without deleting the story it created", async ({
  page,
}) => {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(STRAY);
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: STRAY }),
  ).toBeVisible();

  // No cleanup here on purpose — this test *is* the stranding.
});

test("the stray is gone by the next test, without that test asking", async ({
  page,
}) => {
  await expect(
    page.locator(".card", { hasText: STRAY }),
  ).toHaveCount(0);
});

/**
 * The same rule for a stray that was CLOSED before its test gave up on it
 * (SH-222).
 *
 * `story-context-menu-status.spec.ts` moves a story it created to `done` and
 * deletes it two statements later, so a failure in between strands a closed
 * one — and a closed stray is the worse kind. The board counts it: `/data`
 * excludes only deleted and draft stories, and `#filter-count`'s denominator
 * is `state.data.stories.length`, so `filter-persistence.spec.ts`'s `0 / 2`
 * reads `0 / 3` for the rest of the run. That is the "0 / 3" and "0 / 4"
 * SH-223 reported, and it does not clear on its own the way an open stray
 * did once SH-245's sweep existed.
 */

const CLOSED_STRAY = "SH-222 stray — closed, then never cleaned up";

/** Alpha's story count as the fixture defines it, read before the closed
 * stray exists so the assertion below is against the run's own baseline
 * rather than a number copied out of `run-e2e.sh`. */
let alphaTotalBeforeStray = 0;

async function alphaStories(request: import("@playwright/test").APIRequestContext) {
  const slug = await projectSlug(request, "Alpha Project");
  const resp = await request.get(`/api/repos/${encodeURIComponent(slug)}/data`, {
    headers: { "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN") },
  });
  const data = await resp.json();
  return data.stories as Array<{ story: { id: string; title: string } }>;
}

test("a test can end without deleting the story it closed", async ({
  page,
  request,
}) => {
  alphaTotalBeforeStray = (await alphaStories(request)).length;

  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(CLOSED_STRAY);
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);

  const card = page.locator('.column[data-state="todo"] .card', {
    hasText: CLOSED_STRAY,
  });
  await expect(card).toBeVisible();
  await card.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await page.locator("#drawer-body select").first().selectOption("done");
  await expect(
    page.locator('.column[data-state="done"] .card', { hasText: CLOSED_STRAY }),
  ).toBeVisible();

  // No cleanup here on purpose — this test *is* the stranding.
});

test("the closed stray is gone too, and Alpha's story count is back", async ({
  request,
}) => {
  const stories = await alphaStories(request);
  expect(stories.map((view) => view.story.title)).not.toContain(CLOSED_STRAY);
  // The count, not just the title: the denominator is what an unrelated
  // spec's `0 / 2` assertion actually reads.
  expect(stories.length).toBe(alphaTotalBeforeStray);
});
