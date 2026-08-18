import { test, expect } from "@playwright/test";
import {
  cleanUpCreatedStories,
  createStory,
  deleteStory,
  openProject,
  seedToken,
} from "./support";

/**
 * SH-398: the drawer's blocked banner used to be `["Blocked: "].concat(
 * linkifyStoryIds(st.awaiting)).concat([Unblock])` rendered straight into
 * `.banner`, which was `display: flex; align-items: center` with no
 * `flex-direction: column` and no wrapping body element. `linkifyStoryIds()`
 * returns an *array* of interleaved text nodes and `storyRef()` elements, so
 * each fragment of a multi-sentence reason became its own anonymous flex
 * item -- narrow columns of wrapped text side by side, rather than one
 * wrapped paragraph (the screenshot attached to this story). Fixed by
 * splitting the banner into `.banner-head` (a row: headline + Unblock) and
 * `.banner-body.md` (ordinary block flow, rendered through the same
 * `renderMarkdown()` descriptions and comments already use).
 *
 * `toContainText`/`toHaveText` cannot catch this class of regression: the
 * broken render contains exactly the right characters, just laid out badly.
 * The tests below measure geometry instead.
 */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

async function openDrawer(
  page: import("@playwright/test").Page,
  title: string,
) {
  const detailLoaded = page.waitForResponse(
    (resp) =>
      /\/story\/[^/]+$/.test(new URL(resp.url()).pathname) &&
      resp.request().method() === "GET",
  );
  await page
    .locator('.column[data-state="todo"] .card', { hasText: title })
    .click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await detailLoaded;
}

test("a long, multi-sentence blocked reason wraps as one paragraph, not narrow flex columns", async ({
  page,
}) => {
  const blockerATitle = "SH-398 banner layout — blocker A";
  const blockerBTitle = "SH-398 banner layout — blocker B";
  const workerTitle = "SH-398 banner layout — the blocked story";
  const blockerAId = await createStory(page, blockerATitle);
  const blockerBId = await createStory(page, blockerBTitle);
  await createStory(page, workerTitle);

  const reason =
    "make test-full's webkit e2e leg has a recurring, pre-existing, " +
    "unrelated flake (filed as " +
    blockerAId +
    ") -- two failures in two different tests, both resolved on immediate " +
    "retry with zero code changes. Blocked pending " +
    blockerBId +
    "'s resolution or an explicit human decision on whether the gate-tier " +
    "receipt suffices for this merge.";

  await openDrawer(page, workerTitle);
  await page.locator('input[placeholder="Reason for blocking…"]').fill(reason);
  await page.locator("#drawer-body button", { hasText: "Block" }).click();

  const banner = page.locator(".banner-blocked");
  await expect(banner).toBeVisible();
  const body = banner.locator(".banner-body");
  await expect(body).toBeVisible();
  await expect(body).toContainText(blockerAId);
  await expect(body).toContainText(blockerBId);

  const bannerBox = await banner.boundingBox();
  const bodyBox = await body.boundingBox();
  if (!bannerBox || !bodyBox) {
    throw new Error("expected both the banner and its body to have a layout box");
  }

  // A wrapped paragraph spans nearly the banner's own width; a run of
  // narrow flex-item columns does not. `.banner`'s own horizontal padding
  // accounts for the small gap this threshold leaves.
  expect(bodyBox.width).toBeGreaterThan(bannerBox.width * 0.8);

  // And it must actually wrap onto more than one line, derived from the
  // element's own font size rather than a bare pixel literal (this
  // project's own standing rule on timing/geometry ceilings) -- a single
  // line would be roughly one font-size tall; several wrapped lines of a
  // ~300-character reason are several times that.
  const fontSizePx = await body.evaluate((el) =>
    parseFloat(getComputedStyle(el).fontSize),
  );
  expect(bodyBox.height).toBeGreaterThan(fontSizePx * 2);

  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  await deleteStory(page, workerTitle);
  await deleteStory(page, blockerATitle);
  await deleteStory(page, blockerBTitle);
});

test("the blocked reason renders GFM-lite markdown, the same as descriptions and comments", async ({
  page,
}) => {
  const blockerTitle = "SH-398 banner markdown — the blocker";
  const workerTitle = "SH-398 banner markdown — the blocked story";
  const blockerId = await createStory(page, blockerTitle);
  await createStory(page, workerTitle);

  await openDrawer(page, workerTitle);
  await page
    .locator('input[placeholder="Reason for blocking…"]')
    .fill("**blocked** until " + blockerId + " lands");
  await page.locator("#drawer-body button", { hasText: "Block" }).click();

  const body = page.locator(".banner-blocked .banner-body");
  await expect(body).toBeVisible();
  await expect(body.locator("strong")).toHaveText("blocked");
  // The story id inside the reason is linkified the same way a comment
  // body's is -- a live `storyRef()` chip, not literal text.
  await expect(body.locator(".rel-id", { hasText: blockerId })).toBeVisible();

  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  await deleteStory(page, workerTitle);
  await deleteStory(page, blockerTitle);
});
