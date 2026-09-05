import { test, expect } from "./support";
import { openProject, requiredEnv, seedToken } from "./support";

/**
 * SH-290 — the drawer belongs to the repo screen, and to no other.
 *
 * `closeDrawer()` was hand-placed at three of the four functions that change
 * screen; `goStatuses()` omitted it, so an open drawer survived into the
 * statuses editor — a sub-view of Settings that has nothing to do with the
 * story it was showing.
 *
 * Reaching that state depended on a deep-link race SH-300 has since closed:
 *
 * **A drawer used to be able to open while the user was on Settings.**
 *     `consumeDeepLinkStory()` ran off the first `/data` reply for a repo and
 *     was gated on `state.repoId` alone, which `goSettings()` does not
 *     change — so a `?project=&story=` link whose `/data` was still in
 *     flight when the user reached Settings would open its drawer over
 *     Settings. SH-300 closed this: the same function now also checks
 *     `state.screen`, and refuses (with a durable toast) rather than opening
 *     off-board. That state is therefore no longer reachable at all, and the
 *     test that used to arrange it — `openDrawerOverSettings()` — moved to
 *     `deep-link.spec.ts`, inverted to assert the refusal instead. See that
 *     file's own header and SH-300's `DECISION.md` for the fix and why it
 *     lives where it does.
 * What remains provable here — and still worth its own spec, distinct from
 * `project-selector.spec.ts`'s `selectRepo()` pin — is `renderScreen()`'s
 * derived dismissal rule itself: leaving the repo screen for Home or
 * Settings by the ordinary board route closes an open drawer.
 * SH-554 makes the detail panel a non-modal peer, so these now use the real
 * topbar clicks a person uses; that interaction is part of the contract.
 */

const ALPHA_STORY_ID = requiredEnv("DASHBOARD_ALPHA_STORY_ID");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
});

/**
 * Opens Alpha's board and a story's drawer the ordinary way — a mouse click on
 * a card, no race arranged.
 */
async function openBoardDrawer(
  page: import("@playwright/test").Page,
): Promise<void> {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await page
    .locator(".card-title", { hasText: "Wire up the auth flow" })
    .click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(page.locator("#drawer-id")).toHaveText(ALPHA_STORY_ID);
}

/**
 * The two topbar routes off the board, each pinned before the `closeDrawer()`
 * call inside `goHome()`/`goSettings()` was removed in favour of
 * `renderScreen()`'s derived rule — so the removal was covered rather than
 * merely believed to be.
 */
for (const route of [
  { name: "Home", button: "#home-btn", view: "#home-view" },
  { name: "Settings", button: "#settings-btn", view: "#settings-view" },
]) {
  test(`leaving the board for ${route.name} dismisses the drawer`, async ({
    page,
  }) => {
    await openBoardDrawer(page);

    await page.locator(route.button).click();

    await expect(page.locator(route.view)).toBeVisible();
    await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  });
}
