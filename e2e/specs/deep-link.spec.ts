import { test, expect } from "@playwright/test";
import { holdFetch, openProject, projectSlug, requiredEnv, seedToken } from "./support";

/**
 * Exercises SH-197's `?project=<slug>&story=<id>` deep link against a real
 * daemon and the two seeded projects `project-selector.spec.ts` also uses:
 *
 *   - "Alpha Project" (prefix AA) — has a checkout, two open stories,
 *     including `DASHBOARD_ALPHA_STORY_ID` ("Wire up the auth flow")
 *   - "Gamma Archive" (prefix GA) — `--no-attach`: no checkout, so it's
 *     reachable read-only, exactly the case `resolveDeepLinkProject()`
 *     (`src/web_dashboard.html`) must gate on `canOpen`, not `available`
 *
 * `projectSlug()` (`./support.ts`) asks the daemon for each project's slug
 * rather than assuming the shape `story project new` derives from a name —
 * that derivation is not this suite's to depend on.
 */

const ALPHA_STORY_ID = requiredEnv("DASHBOARD_ALPHA_STORY_ID");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
});

test("?project= lands directly on that project's board, bypassing Home", async ({
  page,
  request,
}) => {
  const alpha = await projectSlug(request, "Alpha Project");
  await page.goto(`/?project=${alpha}`);

  await expect(page.locator("#board-view")).toBeVisible();
  await expect(page.locator("#home-view")).toBeHidden();
  await expect(page.locator("#projsel-btn")).toContainText("AA · Alpha Project");
});

test("?project=&story= also opens that story's drawer", async ({
  page,
  request,
}) => {
  const alpha = await projectSlug(request, "Alpha Project");
  await page.goto(`/?project=${alpha}&story=${ALPHA_STORY_ID}`);

  await expect(page.locator("#board-view")).toBeVisible();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(page.locator("#drawer-id")).toHaveText(ALPHA_STORY_ID);
});

test("an unknown ?project= lands on Home with an error toast", async ({
  page,
}) => {
  await page.goto("/?project=no-such-project-xyz");

  await expect(page.locator("#home-view")).toBeVisible();
  const toast = page.locator("#toast-stack .toast.error");
  await expect(toast).toBeVisible();
  await expect(toast).toContainText("no-such-project-xyz");
});

test("a project with no checkout is still reachable by deep link (canOpen, not available)", async ({
  page,
  request,
}) => {
  const gamma = await projectSlug(request, "Gamma Archive");
  await page.goto(`/?project=${gamma}`);

  await expect(page.locator("#board-view")).toBeVisible();
  await expect(page.locator("#projsel-btn")).toContainText("GA · Gamma Archive");
});

test("?story= naming a story absent from the project errors and is stripped from the address bar", async ({
  page,
  request,
}) => {
  const alpha = await projectSlug(request, "Alpha Project");
  await page.goto(`/?project=${alpha}&story=AA-9999`);

  await expect(page.locator("#board-view")).toBeVisible();
  const toast = page.locator("#toast-stack .toast.error");
  await expect(toast).toBeVisible();
  await expect(toast).toContainText("AA-9999");
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  // `syncUrl()` runs synchronously off the same fetch callback that shows
  // the toast, so by the time the toast above is visible the address bar
  // has already settled -- no retry needed.
  const url = new URL(page.url());
  expect(url.searchParams.get("project")).toBe(alpha);
  expect(url.searchParams.get("story")).toBeNull();
});

test("selecting a project and a story updates the address bar; closing keeps the project", async ({
  page,
  request,
}) => {
  const alpha = await projectSlug(request, "Alpha Project");
  await page.goto("/");
  expect(new URL(page.url()).searchParams.get("project")).toBeNull();

  await openProject(page, "Alpha Project");
  expect(new URL(page.url()).searchParams.get("project")).toBe(alpha);
  expect(new URL(page.url()).searchParams.get("story")).toBeNull();

  await page
    .locator(".card-title", { hasText: "Wire up the auth flow" })
    .click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  expect(new URL(page.url()).searchParams.get("story")).toBe(ALPHA_STORY_ID);

  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  const closedUrl = new URL(page.url());
  expect(closedUrl.searchParams.get("project")).toBe(alpha);
  expect(closedUrl.searchParams.get("story")).toBeNull();
});

test("unrelated query parameters survive selecting a project", async ({
  page,
  request,
}) => {
  const alpha = await projectSlug(request, "Alpha Project");
  await page.goto("/?sseStaleAfterMs=400");

  await openProject(page, "Alpha Project");

  const url = new URL(page.url());
  expect(url.searchParams.get("sseStaleAfterMs")).toBe("400");
  expect(url.searchParams.get("project")).toBe(alpha);
});

/**
 * SH-300 — a deep-linked story that resolves after the user has already left
 * the board is refused, not opened.
 *
 * `consumeDeepLinkStory()` runs off a repo's first `/data` reply and used to
 * call `openDrawer()` unconditionally, with no check on `state.screen`. Since
 * SH-299 that matters more than a rendering overlap: `openDrawer()` marks the
 * entire `.app` shell `inert` and moves focus into the drawer, so a
 * late-resolving link would seize the screen and keyboard out from under
 * whatever the user is doing on Settings at that moment.
 *
 * `openDrawerOverSettings()` moved here from `drawer-screen-scope.spec.ts`
 * (which used it to arrange the very state this fix closes off) and is
 * inverted: same arrangement, opposite outcome. See that file's header for
 * why the state it used to prove is no longer reachable at all.
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
}

test("a deep link that resolves after the user left the board is refused, not opened", async ({
  page,
  request,
}) => {
  const alpha = await projectSlug(request, "Alpha Project");
  await openDrawerOverSettings(page, alpha);

  // Refused: the board the link named never gets its drawer, Settings is
  // still what's on screen, and nothing went `inert` -- the council's own
  // criterion (SH-300's DECISION.md) for why this can't be a second guard
  // inside openDrawer() itself: the shell only goes inert when an overlay
  // actually opens.
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  await expect(page.locator("#settings-view")).toBeVisible();
  await expect(page.locator("#app")).not.toHaveAttribute("inert", "");

  // Reported, not silently dropped: `syncUrl()` has already stripped
  // `?project=&story=` by the time this fires (the Settings navigation ran
  // it), so the toast is the only remaining record that a valid link existed.
  const toast = page.locator("#toast-stack .toast.error");
  await expect(toast).toBeVisible();
  await expect(toast).toContainText(ALPHA_STORY_ID);
});
