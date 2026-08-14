import { test, expect } from "@playwright/test";
import { activateBehindOverlay, openProject, requiredEnv, seedToken } from "./support";

/**
 * SH-290 — the drawer belongs to the repo screen, and to no other.
 *
 * `closeDrawer()` was hand-placed at three of the four functions that change
 * screen; `goStatuses()` omitted it, so an open drawer survived into the
 * statuses editor — a sub-view of Settings that has nothing to do with the
 * story it was showing.
 *
 * Reaching that state took two facts about this file, one of which SH-300
 * has since closed:
 *
 *  1. **A drawer used to be able to open while the user was on Settings.**
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
 *  2. **The Statuses button was unreachable by hand in that state, and was
 *     not always.** `.backdrop` is `position: fixed; inset: 0`, so a pointer
 *     click lands on it and closes the drawer. Until SH-299 nothing marked
 *     the background `inert`, so the same button kept its place in the tab
 *     order and Enter activated it — the dashboard was modal for one input
 *     device and not the other. SH-299 closed that gap too, correctly; this
 *     fact is preserved here only because it still governs the two tests
 *     below, which drive Home/Settings the same synthetic way.
 *
 * What remains provable here — and still worth its own spec, distinct from
 * `project-selector.spec.ts`'s `selectRepo()` pin — is `renderScreen()`'s
 * derived dismissal rule itself: leaving the repo screen for Home or
 * Settings by the ordinary board route closes an open drawer.
 *
 * Synthetic activation (`activateBehindOverlay()`) is still required for
 * both: with the drawer open, `.backdrop` intercepts a real click on either
 * topbar button, and the click that does land is the backdrop's own
 * dismissal, which closes the drawer without ever running the navigation
 * under test. Since SH-299 the keyboard fares no better, by design — see
 * that helper's own comment.
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

    await activateBehindOverlay(page.locator(route.button));

    await expect(page.locator(route.view)).toBeVisible();
    await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  });
}
