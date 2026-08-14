import { test, expect } from "@playwright/test";
import { holdFetch, projectSlug, requiredEnv, seedToken } from "./support";

/**
 * SH-290 — the drawer belongs to the repo screen, and to no other.
 *
 * `closeDrawer()` was hand-placed at three of the four functions that change
 * screen; `goStatuses()` omitted it, so an open drawer survived into the
 * statuses editor — a sub-view of Settings that has nothing to do with the
 * story it was showing.
 *
 * Reaching that state takes two facts about this file that are easy to miss
 * and are the whole reason this spec is shaped the way it is:
 *
 *  1. **A drawer can open while the user is on Settings.**
 *     `consumeDeepLinkStory()` runs off the first `/data` reply for a repo
 *     and is gated on `state.repoId`, which `goSettings()` does not change —
 *     so a `?project=&story=` link whose `/data` is still in flight when the
 *     user reaches Settings opens its drawer over Settings.
 *  2. **The Statuses button is unreachable by mouse in that state, and
 *     perfectly reachable by keyboard.** `.backdrop` is `position: fixed;
 *     inset: 0`, so a pointer click lands on it and closes the drawer — but
 *     nothing marks the background `inert`, so the button keeps its place in
 *     the tab order and Enter activates it. `press()` models that user, and
 *     is not a workaround for an actionability check: a hit-tested click
 *     would exercise the backdrop's own dismissal instead of `goStatuses()`.
 */

const ALPHA_STORY_ID = requiredEnv("DASHBOARD_ALPHA_STORY_ID");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
});

/**
 * Opens Alpha's board by deep link with its first `/data` held, leaves for
 * Settings while that reply is still in flight, then delivers it — landing
 * the deep-linked drawer on a screen it does not belong to.
 *
 * `sealOnHold` refuses every later `/data`: `consumeDeepLinkStory()` fires
 * only on a repo's *first* load, so a second reply landing first would set
 * `state.data` and make the held one a no-op — a spec that passed having
 * arranged nothing.
 */
async function openDrawerOverSettings(
  page: import("@playwright/test").Page,
  alpha: string,
): Promise<void> {
  const held = await holdFetch(
    page,
    (url) => url.pathname.endsWith("/data"),
    () => true,
    { sealOnHold: true },
  );

  await page.goto(`/?project=${alpha}&story=${ALPHA_STORY_ID}`);
  await held.taken;

  await page.locator("#settings-btn").click();
  await expect(page.locator("#settings-view")).toBeVisible();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  await held.deliver();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(page.locator("#drawer-id")).toHaveText(ALPHA_STORY_ID);
}

test("a drawer open over Settings does not survive into the statuses sub-view", async ({
  page,
  request,
}) => {
  const alpha = await projectSlug(request, "Alpha Project");
  await openDrawerOverSettings(page, alpha);

  await page
    .locator(".settings-table tbody tr", { hasText: "Alpha Project" })
    .getByRole("button", { name: "Statuses" })
    .press("Enter");

  await expect(page.locator(".settings-head h2")).toHaveText(
    "Statuses · Alpha Project",
  );
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
});
