import { test, expect } from "@playwright/test";
import { holdFetch, openProject, projectSlug, requiredEnv, seedToken } from "./support";

/**
 * Exercises SH-197's `?project=<slug>&story=<id>` deep link against a real
 * daemon and the three seeded projects `project-selector.spec.ts` also uses:
 *
 *   - "Alpha Project" (prefix AA) — has a checkout, two open stories,
 *     including `DASHBOARD_ALPHA_STORY_ID` ("Wire up the auth flow")
 *   - "Beta Project" (prefix BB) — has a checkout, one open story
 *     ("Draft the release notes") — SH-300's project-mismatch spec switches
 *     here mid-flight
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
  await expectNoDiagnosis(toast);
});

/**
 * A client-only error notice is a headline and nothing else — SH-367's
 * negative half, and the part of that verdict that would otherwise decay in
 * silence.
 *
 * Eight notices in this dashboard are raised without any request having
 * failed: an unknown `?project=`, a `?story=` the project does not hold, a
 * deep link the user navigated away from, a clipboard refusal, a project
 * deleted in another tab, a status with nowhere to move its stories. Their
 * headline already contains everything known — two of them fold their only
 * detail into it — so there is no diagnosis to add, and SH-367's council ruled
 * that none is owed.
 *
 * That ruling is prose, and prose decays. `.notice-detail` and
 * `.notice-reason` are rendered by `noticeBody` for *every* notice, so a later
 * change that starts passing a third argument at one of these sites would
 * bolt a fabricated diagnosis onto a notice that has nothing to diagnose, and
 * nothing in the suite would notice. This is the assertion that refuses it.
 *
 * Deliberately the inverse shape of the pin in
 * `drawer-field-mutation-timeout.spec.ts`: there a subject line MUST appear,
 * because a failed request has an identity worth naming. Here it must not,
 * because there was no request.
 */
async function expectNoDiagnosis(
  toast: import("@playwright/test").Locator,
): Promise<void> {
  await expect(toast.locator(".notice-headline")).toHaveCount(1);
  await expect(toast.locator(".notice-detail")).toHaveCount(0);
  await expect(toast.locator(".notice-reason")).toHaveCount(0);
}

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
  await expectNoDiagnosis(toast);
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
  await expectNoDiagnosis(toast);
});

/**
 * SH-300's sibling defect: a pending deep link belongs to the project it
 * named, not to whichever project's board happens to load first.
 *
 * Before this fix, `pendingDeepLinkStory` was a bare story id with no
 * project of its own. Switching projects during the loading window didn't
 * clear it -- `fetchData()` drops a stale reply by comparing `askedFor`
 * against `state.repoId`, but that check runs *before*
 * `consumeDeepLinkStory()` and never touches the pending link -- so the
 * *next* project's own first load consumed it, searched its own stories for
 * an id that belonged to a different project, and reported
 * "No story `<id>` in <wrong project>", blaming the project the user
 * switched to for a story it was never asked about.
 *
 * `held`'s `matches` is scoped to Alpha's own `/data` endpoint specifically
 * (`/api/repos/<alpha>/data`), not every project's -- generically matching
 * any `/data` reply races Alpha's own initial load against the SSE
 * connection's onopen resync fetch for the *same* project, and whichever of
 * those two identical-looking requests happens to arrive at the route
 * handler first becomes "the held one," leaving the other free to land and
 * open the drawer before this spec ever gets to switch projects. Scoping to
 * Alpha's URL means either of Alpha's own requests is a fine one to hold --
 * both satisfy `isFirstLoadForRepo` identically -- while leaving Beta's own
 * `/data` endpoint (a different URL) alone entirely, so its board loads
 * normally without needing to reason about the race a second time.
 * `sealOnHold` closes the same race a second way: once the first Alpha
 * reply is held, a second one landing unheld first would set `state.data`
 * and make the held one a no-op.
 */
test("a deep link pending for one project is not consumed by a different project's load", async ({
  page,
  request,
}) => {
  const alpha = await projectSlug(request, "Alpha Project");
  const alphaDataPath = `/api/repos/${alpha}/data`;
  const held = await holdFetch(
    page,
    (url) => url.pathname === alphaDataPath,
    () => true,
    { sealOnHold: true },
  );

  await page.goto(`/?project=${alpha}&story=${ALPHA_STORY_ID}`);
  await held.taken;

  await page.locator("#projsel-btn").click();
  await page
    .locator("#projsel-menu .projsel-item", { hasText: "Beta Project" })
    .click();
  await expect(
    page.locator(".card-title", { hasText: "Draft the release notes" }),
  ).toBeVisible();

  // Beta's own first load fires `consumeDeepLinkStory()` while the link is
  // still pending -- refused and reported, naming ALPHA, never Beta.
  const toast = page.locator("#toast-stack .toast.error");
  await expect(toast).toBeVisible();
  await expect(toast).toContainText(ALPHA_STORY_ID);
  await expectNoDiagnosis(toast);
  await expect(toast).toContainText(alpha);
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  // Alpha's held reply, delivered last, is now a stale/wrong-project reply
  // by `fetchData()`'s own ticket check -- dropped before it can touch
  // anything, including the (already-cleared) pending link.
  await held.deliver();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
});
