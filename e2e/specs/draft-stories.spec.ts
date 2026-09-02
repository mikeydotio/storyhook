import { test, expect } from "./support";
import {
  cleanUpCreatedStories,
  deleteStory,
  latch,
  openProject,
  seedToken,
} from "./support";

/**
 * Exercises SH-175: the New Story modal's redesigned footer (Discard
 * Draft / Save Draft / Create Story), the removal of its stray-dismiss
 * behavior (backdrop click, Escape), and the Drafts popover — count badge,
 * row list, and reusing the New Story modal as "Edit draft".
 *
 * Fixture: "Alpha Project" (prefix AA), shared across the whole e2e run —
 * every test here cleans up whatever it creates (deletes or publishes any
 * draft it leaves behind) so sibling specs relying on this project's story
 * count (e.g. filter-persistence.spec.ts) are not disturbed.
 */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

test("the New Story modal shows Discard Draft, Save Draft and Create Story, in that order", async ({
  page,
}) => {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await expect(page.locator("#create-modal-header")).toHaveText("New story");

  const buttons = page.locator("#create-modal .modal-footer button");
  await expect(buttons).toHaveCount(3);
  await expect(buttons.nth(0)).toHaveText("Discard Draft");
  await expect(buttons.nth(1)).toHaveText("Save Draft");
  await expect(buttons.nth(2)).toHaveText("Create Story");

  await page.locator("#create-discard").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
});

test("clicking the backdrop does not dismiss the New Story modal", async ({
  page,
}) => {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);

  // Click the backdrop itself, well away from the modal's own box.
  await page.locator("#modal-backdrop").click({ position: { x: 4, y: 4 } });
  await expect(page.locator("#create-modal")).toHaveClass(/open/);

  await page.locator("#create-discard").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
});

test("pressing Escape does not dismiss the New Story modal", async ({
  page,
}) => {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);

  await page.keyboard.press("Escape");
  await expect(page.locator("#create-modal")).toHaveClass(/open/);

  await page.locator("#create-discard").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
});

test("Discard Draft on an unsaved new story creates nothing", async ({
  page,
}) => {
  const title = "Never actually created";
  await page.locator("#new-story-btn").click();
  await page.locator("#create-title").fill(title);
  await page.locator("#create-discard").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);

  await expect(
    page.locator(".card", { hasText: title }),
  ).toHaveCount(0);
});

test("Save Draft creates a draft: it is not a board card, and the Drafts button counts it", async ({
  page,
}) => {
  const title = "A sketch worth keeping";
  // SH-235: #drafts-btn's accessible name is now an icon span plus this
  // text span (icon-only at narrow widths, its own text hidden there via
  // .sr-only-equivalent CSS -- see .icon-compact-btn) -- the count text
  // itself lives on the label span, not the button's own concatenated
  // textContent.
  const draftsBtn = page.locator("#drafts-btn");
  const draftsBtnText = page.locator("#drafts-btn-text");
  const before = await draftsBtnText.textContent();

  await page.locator("#new-story-btn").click();
  await page.locator("#create-title").fill(title);
  await page.locator("#create-save-draft").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);

  // Not a board card anywhere.
  await expect(page.locator(".card", { hasText: title })).toHaveCount(0);
  // The Drafts button's count went up by exactly one.
  await expect(draftsBtnText).not.toHaveText(before ?? "");
  const beforeCount = parseInt((before ?? "0").split(" ")[0], 10) || 0;
  await expect(draftsBtnText).toHaveText(`${beforeCount + 1} Drafts`);

  // Cleanup via the popover, exercised properly by the next test — here,
  // just discard it directly so this test doesn't depend on that one.
  await draftsBtn.click();
  await expect(page.locator("#drafts-modal")).toHaveClass(/open/);
  await page
    .locator("#drafts-list .drafts-row", { hasText: title })
    .click();
  await expect(page.locator("#create-modal-header")).toHaveText("Edit draft");
  await page.locator("#create-discard").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  await expect(draftsBtnText).toHaveText(`${beforeCount || "No"} Drafts`);
});

test("a saved draft leaves the catalog pending until its fresh row arrives", async ({
  page,
}) => {
  const title = "A draft held at the catalog boundary";
  const catalogGate = latch();
  const catalogStarted = latch();
  let holdCatalog = false;
  await page.route(
    (url) => url.pathname === "/api/repos",
    async (route) => {
      if (route.request().method() !== "GET" || !holdCatalog) {
        await route.continue();
        return;
      }
      catalogStarted.release();
      await catalogGate.held;
      await route.continue();
    },
  );

  await page.locator("#new-story-btn").click();
  await page.locator("#create-title").fill(title);
  holdCatalog = true;
  await page.locator("#create-save-draft").click();
  await catalogStarted.held;
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);

  await expect(page.locator("#drafts-btn-text")).toHaveText("Drafts");
  await page.locator("#drafts-btn").click();
  await expect(page.locator("#drafts-list")).toHaveText("Loading drafts…");

  holdCatalog = false;
  catalogGate.release();
  await expect(
    page.locator("#drafts-list .drafts-row", { hasText: title }),
  ).toBeVisible();
});

test("the Drafts popover dismisses on an outside click, unlike the New Story modal", async ({
  page,
}) => {
  await page.locator("#drafts-btn").click();
  await expect(page.locator("#drafts-modal")).toHaveClass(/open/);

  await page.locator("#drafts-backdrop").click({ position: { x: 4, y: 4 } });
  await expect(page.locator("#drafts-modal")).not.toHaveClass(/open/);
});

/**
 * `closeDraftsModal()` hides the backdrop on a 150ms timer, so the fade-out
 * has something to fade. That delayed write used to be unconditional — it
 * trusted what was true when it was scheduled — and the Escape handler calls
 * `closeDraftsModal()` on every press, open or not. So pressing Escape and
 * then opening the popover inside that window landed the hide on the *open*
 * popover: `<div id="drafts-backdrop" class="backdrop open" hidden>`, which
 * is invisible and unclickable, with the popover itself still open above it.
 */
test("a popover opened inside the previous close's fade-out keeps its backdrop", async ({
  page,
}) => {
  // Both halves in one JavaScript tick, so the timer is guaranteed to fire
  // after the popover is open rather than whenever the machine allows —
  // driven as separate Playwright actions this straddles the 150ms and
  // passes or fails on load.
  await page.evaluate(() => {
    document.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
    );
    (document.getElementById("drafts-btn") as HTMLElement).click();
  });
  await expect(page.locator("#drafts-modal")).toHaveClass(/open/);

  // Past the timer deliberately. Asserting immediately would pass even
  // against the bug, since the backdrop is genuinely visible until the stale
  // timer fires at 150ms — the assertion has to outlive it to mean anything.
  await page.waitForTimeout(250);
  await expect(page.locator("#drafts-backdrop")).toBeVisible();

  // And the guard must not have pinned it open the other way.
  await page.locator("#drafts-backdrop").click({ position: { x: 4, y: 4 } });
  await expect(page.locator("#drafts-modal")).not.toHaveClass(/open/);
});

test("publishing a draft from the Edit Draft modal makes it a live board card", async ({
  page,
}) => {
  const title = "Ready to ship";

  await page.locator("#new-story-btn").click();
  await page.locator("#create-title").fill(title);
  await page.locator("#create-save-draft").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  await expect(page.locator(".card", { hasText: title })).toHaveCount(0);

  await page.locator("#drafts-btn").click();
  await page
    .locator("#drafts-list .drafts-row", { hasText: title })
    .click();
  await expect(page.locator("#create-modal-header")).toHaveText("Edit draft");
  await expect(page.locator("#create-submit")).toHaveText("Publish");
  await expect(page.locator("#create-title")).toHaveValue(title);

  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);

  await expect(page.locator(".card", { hasText: title })).toBeVisible();

  // Cleanup: a live story, so the ordinary drawer delete flow applies.
  await deleteStory(page, title);
});
