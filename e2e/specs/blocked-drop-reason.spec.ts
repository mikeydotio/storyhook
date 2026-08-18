import { test, expect } from "./support";
import { cleanUpCreatedStories, openProject, seedToken } from "./support";

/**
 * Exercises SH-205: dragging a card into the Blocked column opens a
 * skippable prompt for the `awaiting` reason, and the move completes either
 * way — submitting a reason, skipping, or dismissing via Escape all move
 * the card; only whether `awaiting` ends up set differs. Decided by council
 * vote (verdict on SH-205, unanimous in
 * round 1): reason-capture on `move` is strictly opt-in, threaded through
 * the same atomic `extra` seam `comment` already uses, and never gates the
 * move itself.
 *
 * This spec creates and deletes its own stories rather than touching the
 * "Alpha Project" fixture, whose exact two-story shape other specs
 * (filter-persistence.spec.ts, column-visibility.spec.ts) assert on
 * byte-for-byte per run-e2e.sh's own comment.
 *
 * The "submitted" case's badge assertion now quotes the recorded reason
 * (SH-309, `blockedFlag()` in `status-flags.spec.ts`'s own header comment)
 * -- an awaiting reason is a cause like any other, so it no longer
 * disappears into a bare "● blocked". The two "(no reason)" cases below
 * are untouched: a card left in Blocked with no relationships and no
 * `awaiting` genuinely has none to show.
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
  // SH-309: an awaiting reason is itself a cause, quoted in the badge's
  // parenthetical, since a reason is free text a reader wrote, not a
  // relationship record. (The drawer's own banner body DOES linkify a
  // reason's story ids -- pinned by blocked-banner-layout.spec.ts, which
  // uses an id this project actually holds. The id here carries a foreign
  // prefix on purpose, so it stays literal text on every surface.)
  await expect(card.locator(".flag-blocked")).toHaveText(
    '● blocked ("waiting on SH-9")',
  );

  await card.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  // SH-398 split the banner into `.banner-head` -- the headline plus the
  // Unblock button -- above `.banner-body`, the reason as rendered
  // markdown; there is no "Blocked: <reason>" text run joining the two any
  // more (docs/spec/blocked-causes.md, "The dashboard"). So each half is
  // asserted where it actually lives. Asserting the container instead
  // concatenates them into "BlockedUnblockwaiting on SH-9" -- which is
  // what left this spec red on main from SH-398 until SH-416.
  const banner = page.locator(".banner-blocked");
  await expect(banner.locator(".banner-head")).toContainText("Blocked");
  // This story HAS an awaiting reason, so the Unblock button renders --
  // the complement of status-flags.spec.ts's relation-only banner, which
  // asserts the same button is absent and that there is no body at all.
  await expect(banner.locator("button", { hasText: "Unblock" })).toBeVisible();
  await expect(banner.locator(".banner-body")).toHaveText("waiting on SH-9");
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
