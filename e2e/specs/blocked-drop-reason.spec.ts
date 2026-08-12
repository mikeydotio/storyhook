import { test, expect } from "@playwright/test";
import { seedToken } from "./support";

/**
 * Exercises SH-205: dragging a card into the Blocked column opens a
 * skippable prompt for the `awaiting` reason, and the move completes either
 * way — submitting a reason, skipping, or dismissing via Escape all move
 * the card; only whether `awaiting` ends up set differs. Decided by council
 * vote (`.council/sh205-block-reason-capture/DECISION.md`, unanimous in
 * round 1): reason-capture on `move` is strictly opt-in, threaded through
 * the same atomic `extra` seam `comment` already uses, and never gates the
 * move itself.
 *
 * This spec creates and deletes its own stories rather than touching the
 * "Alpha Project" fixture, whose exact two-story shape other specs
 * (filter-persistence.spec.ts, column-visibility.spec.ts) assert on
 * byte-for-byte per run-e2e.sh's own comment.
 */

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await expect(page.locator("#board-view")).toBeVisible();
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
  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: title }),
  ).toBeVisible();
}

async function deleteStory(
  page: import("@playwright/test").Page,
  title: string,
  column = "blocked",
) {
  const card = page.locator(`.column[data-state="${column}"] .card`, {
    hasText: title,
  });
  await card.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await page.locator("#drawer-footer button", { hasText: "Delete" }).click();
  await expect(page.locator("#delete-modal")).toHaveClass(/open/);
  await page.locator("#delete-reason").fill("e2e cleanup");
  await page.locator("#delete-modal-submit").click();
  await expect(card).not.toBeVisible();
}

async function dragIntoBlocked(
  page: import("@playwright/test").Page,
  title: string,
) {
  const card = page.locator('.column[data-state="todo"] .card', {
    hasText: title,
  });
  const target = page.locator('.column[data-state="blocked"]');
  await card.dragTo(target);
  await expect(page.locator("#drop-blocked-reason-modal")).toHaveClass(
    /open/,
  );
}

test("submitting a reason on drop moves the card and records it", async ({
  page,
}) => {
  const title = "SH-205 drop reason — submitted";
  await createStory(page, title);
  await dragIntoBlocked(page, title);

  await page
    .locator("#drop-blocked-reason-input")
    .fill("waiting on SH-9");
  await page.locator("#drop-blocked-reason-submit").click();
  await expect(page.locator("#drop-blocked-reason-modal")).not.toHaveClass(
    /open/,
  );

  const card = page.locator('.column[data-state="blocked"] .card', {
    hasText: title,
  });
  await expect(card).toBeVisible();
  await expect(card.locator(".flag-blocked")).toHaveText("● blocked");

  await card.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  // `.banner-blocked` also carries an "Unblock" button, so this checks the
  // reason text is present rather than asserting the element's full text.
  await expect(page.locator(".banner-blocked")).toContainText(
    "Blocked: waiting on SH-9",
  );
  await page.locator("#drawer-close").click();

  await deleteStory(page, title);
});

test("skipping the prompt still moves the card, with no reason recorded", async ({
  page,
}) => {
  const title = "SH-205 drop reason — skipped";
  await createStory(page, title);
  await dragIntoBlocked(page, title);

  await page.locator("#drop-blocked-reason-skip").click();
  await expect(page.locator("#drop-blocked-reason-modal")).not.toHaveClass(
    /open/,
  );

  const card = page.locator('.column[data-state="blocked"] .card', {
    hasText: title,
  });
  await expect(card).toBeVisible();
  await expect(card.locator(".flag-blocked")).toHaveText(
    "● blocked (no reason)",
  );

  await deleteStory(page, title);
});

test("dismissing via Escape still completes the move — the prompt never gates the drop", async ({
  page,
}) => {
  const title = "SH-205 drop reason — escaped";
  await createStory(page, title);
  await dragIntoBlocked(page, title);

  await page.keyboard.press("Escape");
  await expect(page.locator("#drop-blocked-reason-modal")).not.toHaveClass(
    /open/,
  );

  const card = page.locator('.column[data-state="blocked"] .card', {
    hasText: title,
  });
  await expect(card).toBeVisible();
  await expect(card.locator(".flag-blocked")).toHaveText(
    "● blocked (no reason)",
  );

  await deleteStory(page, title);
});
