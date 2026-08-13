import { test, expect } from "@playwright/test";
import { cleanUpCreatedStories, deleteStory, openProject, seedToken } from "./support";

/**
 * Exercises SH-203's status-light component: a coloured dot immediately
 * before an ID wherever the dashboard names another story, coloured to
 * match that story's own board column via a *semantic* stateColor() --
 * not the pre-SH-203 positional palette, under which a closed story could
 * land on any colour at all depending on where its slug happened to sort
 * in the project's configured state order (with the default catalog,
 * `done` sat at index 3 and painted every completed story red) -- and
 * linkifyStoryIds(), which turns a bare mention of another story inside
 * free text (a comment body) into the same light-plus-link pair.
 *
 * Only Playwright can prove the thing that matters here: an actual
 * resolved colour. tests/web_test.rs pins the source text and the CSS
 * rules exist; this is the layer that proves a browser actually resolves
 * "done" to green.
 *
 * This spec creates and deletes its own stories rather than touching the
 * "Alpha Project" fixture, whose exact two-story shape other specs
 * (filter-persistence.spec.ts, column-visibility.spec.ts) assert on
 * byte-for-byte per run-e2e.sh's own comment.
 */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

async function createStory(
  page: import("@playwright/test").Page,
  title: string,
) {
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

/** Moves `title`'s story to `stateSlug` via the drawer's own State select
 * -- not scoped to any one column, since it's also used to move a story
 * back out of wherever an earlier call left it. */
async function moveToState(
  page: import("@playwright/test").Page,
  title: string,
  stateSlug: string,
) {
  await page.locator(".card", { hasText: title }).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await page.locator("#drawer-body select").first().selectOption(stateSlug);
  await expect(
    page.locator('.column[data-state="' + stateSlug + '"] .card', {
      hasText: title,
    }),
  ).toBeVisible();
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
}

/** Adds a relation from whichever story's drawer is currently open to
 * `otherId`, through the Relationships section's own inline-add row --
 * the same `/relate` call a user driving the UI would make. The select is
 * scoped to `.inline-add` because it's the only `<select>` any of the
 * drawer's several `.inline-add` rows (labels, the block-reason form)
 * contains; the button is disambiguated from the block form's "Block" by
 * its own "Add" text. */
async function addRelation(
  page: import("@playwright/test").Page,
  relation: string,
  otherId: string,
) {
  await page.locator('input[placeholder="Story ID (e.g. SH-2)"]').fill(otherId);
  await page.locator("#drawer-body .inline-add select").selectOption(relation);
  await page
    .locator("#drawer-body .inline-add button", { hasText: "Add" })
    .click();
  await expect(page.locator(".rel-row", { hasText: otherId })).toBeVisible();
}

/** The browser's own resolved colour for a CSS custom property, read off a
 * throwaway element rather than assumed -- the light/dark palette differs,
 * and this suite should keep working under whichever colour scheme it
 * actually runs in. */
async function resolvedTokenColor(
  page: import("@playwright/test").Page,
  token: string,
): Promise<string> {
  return page.evaluate((t) => {
    const probe = document.createElement("div");
    probe.style.background = `var(${t})`;
    document.body.appendChild(probe);
    const color = getComputedStyle(probe).backgroundColor;
    probe.remove();
    return color;
  }, token);
}

test("a relation's status light matches the referenced story's column, not its position in the catalog", async ({
  page,
}) => {
  const a = "SH-203 status light — relation source";
  const b = "SH-203 status light — relation target (done)";
  const aCard = await createStory(page, a);
  await createStory(page, b);
  await moveToState(page, b, "done");
  const bId = (await page
    .locator('.column[data-state="done"] .card', { hasText: b })
    .getAttribute("data-id"))!;

  await aCard.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await addRelation(page, "relates-to", bId);

  const relRow = page.locator(".rel-row", { hasText: bId });
  const light = relRow.locator(".story-light");
  // The status word is reachable off-visual, exactly as the story's own
  // "alt/hover text ... returns that status's string" asks for.
  await expect(light).toHaveAttribute("aria-label", "Status: done");
  // The semantic anchor, not the positional palette: `done` is a CLOSED
  // state, so its light must resolve to --success (green) regardless of
  // where "done" happens to sort among this project's configured states.
  await expect(light).toHaveCSS(
    "background-color",
    await resolvedTokenColor(page, "--success"),
  );

  await relRow.locator(".rel-id").click();
  await expect(page.locator("#drawer-id")).toHaveText(bId);

  await page.locator("#drawer-close").click();
  await moveToState(page, b, "todo");
  await deleteStory(page, a);
  await deleteStory(page, b);
});

test("the Done column's own dot is green, proving the semantic palette rather than the positional one", async ({
  page,
}) => {
  const dot = page.locator('.column[data-state="done"] .column-dot');
  await expect(dot).toHaveCSS(
    "background-color",
    await resolvedTokenColor(page, "--success"),
  );
});

test("a comment naming a real story renders a lit link; an unresolvable id and a self-reference stay plain text", async ({
  page,
}) => {
  const source = "SH-203 status light — comment linkification source";
  const target = "SH-203 status light — comment linkification target";
  const sourceCard = await createStory(page, source);
  const targetCard = await createStory(page, target);
  const targetId = (await targetCard.getAttribute("data-id"))!;

  await sourceCard.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  const sourceId = (await page.locator("#drawer-id").textContent())!;
  // Same prefix as a real id, but a number nothing in a fresh project ever
  // reaches -- exercises the "resolves against the prefix, but not in
  // state.data.stories" branch, distinct from a wrong-prefix string
  // (which linkifyStoryIds' own regex would never even consider a match).
  const unknownId = `${sourceId.split("-")[0]}-999999`;

  await page
    .locator('textarea[placeholder="Add a comment…"]')
    .fill(`see ${targetId} and ${unknownId} and ${sourceId} itself`);
  await page.locator("#drawer-body .comment-add button").click();

  const commentText = page.locator(".comment-text").first();
  await expect(commentText).toContainText(unknownId);
  await expect(commentText).toContainText(sourceId);
  const lit = commentText.locator(".rel-id");
  await expect(lit).toHaveCount(1);
  await expect(lit).toHaveText(targetId);

  await page.locator("#drawer-close").click();
  await deleteStory(page, source);
  await deleteStory(page, target);
});
