import { test, expect } from "./support";
import {
  cleanUpCreatedStories,
  deleteStory,
  openProject,
  seedToken,
} from "./support";

/**
 * Exercises SH-424: `populateCard()` updates the two classes it derives from
 * current story state without replacing classes owned by drag, entrance,
 * FLIP, or change-flash lifecycles. The witness uses a second story's real
 * create flow to force an unrelated `/data` render over the retained card.
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
  await page.locator("#create-priority").selectOption("medium");
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: title }),
  ).toBeVisible();
}

test("an unrelated render preserves a card's transient classes (SH-424)", async ({
  page,
}) => {
  const targetTitle = "SH-424 transient class target";
  const triggerTitle = "SH-424 unrelated render trigger";
  const transientClasses = [
    "dragging",
    "entering",
    "moving",
    "flash-priority",
    "future-transient",
  ];

  await createStory(page, targetTitle);
  const target = page.locator('.column[data-state="todo"] .card', {
    hasText: targetTitle,
  });
  await target.evaluate(
    (node, classes) => node.classList.add(...classes),
    transientClasses,
  );

  await createStory(page, triggerTitle);

  await expect(target).toHaveClass(/\bcard\b/);
  await expect
    .poll(() =>
      target.evaluate((node, classes) =>
        classes.filter((name) => !node.classList.contains(name)),
        transientClasses,
      ),
    )
    .toEqual([]);

  await target.evaluate(
    (node, classes) => node.classList.remove(...classes),
    transientClasses,
  );
  await deleteStory(page, triggerTitle);
  await deleteStory(page, targetTitle);
});
