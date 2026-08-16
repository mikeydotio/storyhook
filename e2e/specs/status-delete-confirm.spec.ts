import { expect, test } from "@playwright/test";
import type { APIRequestContext, Locator, Page } from "@playwright/test";
import {
  onAFrozenClock,
  projectSlug,
  requiredEnv,
  seedToken,
} from "./support";

/**
 * The statuses editor's confirmation for deleting an empty status (SH-324).
 *
 * The subject is WCAG 2.2 SC 2.2.1 (Timing Adjustable), the same criterion
 * SH-322 answered for the notice clock — and, per the accessibility seat that
 * found both, the stronger exposure of the two: 2.2.1's central case is a time
 * limit on **completing an action**, which this is, where a notice's clock is a
 * limit on *reading* something corroborated elsewhere on the page.
 *
 * `DELETE_CONFIRM_TIMEOUT_MS = 6000` disarmed the confirmation six seconds
 * after it was armed. A reader who was interrupted, or who simply needs longer
 * than six seconds to move a pointer or find the key, returned to a button that
 * had silently changed its mind — and their next click, which they believed
 * completed the deletion, only re-armed it.
 *
 * ## What these tests are pinning, and what they are not
 *
 * The council settled the route unanimously: **the clock is deleted, not made
 * adjustable** — no preference, no `storyhook.keepNotices` sharing, no Adjust
 * control. So there is no mechanism here to exercise the way
 * `notification-contract.spec.ts` exercises `keepNotices`; there is an absence
 * to prove, which takes a different shape. Two things follow:
 *
 * 1. **The deadline that mattered was not only the timer.** The armed state
 *    lived on the DOM node, and at least eight paths rebuild these rows —
 *    including a second, independent 25s path through `fetchReposOnce()` and
 *    both of `statusMutation()`'s callbacks, which consult no busy guard at all.
 *    Deleting the `setTimeout` alone would have swapped a visible six-second
 *    limit for an invisible 0–25s one. `armedSurvivesAnUnaskedRefresh` below is
 *    the pin for that half, and it is the half a naive suite would miss.
 * 2. **This suite is Chromium-only** (`playwright.config.ts` ships Desktop
 *    Chrome and Pixel 7, both Blink; `make e2e-install` installs chromium
 *    alone). The original defect bit hardest on WebKit, which does not focus a
 *    `<button>` on click — so `document.activeElement` stays on `BODY`,
 *    `statusEditorIsBusy()` reads false, and the poll wipes the confirmation.
 *    **No test in this file observes Safari.** The one below reproduces that
 *    *state* on Blink by blurring after arming, and says so in its own body, so
 *    nobody reads a green run here as evidence about Safari. Adding a `webkit`
 *    project is filed separately and deliberately not smuggled in here.
 *
 * Full reasoning, the vote, and the rejected alternatives: SH-324.
 */

/** Two statuses this file creates and destroys, in a project no other spec
 * asserts a column shape for.
 *
 * Not seeded fixtures: `run-e2e.sh`'s projects are shared, and Alpha's exact
 * two-story four-empty-column shape is itself asserted byte-for-byte by
 * `filter-persistence.spec.ts` and `column-visibility.spec.ts` (which is why
 * Delta exists at all). Two, not one, because "arming a second row disarms the
 * first" is one of the properties under test.
 *
 * The names share no prefix that is a prefix of the other: Playwright's
 * `hasText` is a substring match, so `sh324-scratch` and `sh324-scratch-b`
 * would both match a filter for the first, and every row locator in this file
 * would silently address two rows. */
const SCRATCH_A = "sh324-alpha";
const SCRATCH_B = "sh324-beta";

/** The project the scratch statuses are minted in.
 *
 * Delta, and **not** Gamma, which is the obvious pick and does not work: Gamma
 * is the deliberately unattached project, and the daemon refuses every write to
 * a project with no checkout on this machine — "there is nowhere to run this
 * change: a write fires the project's event hooks and its git operations in its
 * working directory" — so seeding a status there answers 422, not 201.
 *
 * Delta has a checkout and exists precisely to absorb this kind of fixture
 * traffic (SH-208). The two specs that name it assert nothing about its status
 * vocabulary. */
const SCRATCH_PROJECT = "Delta Project";

/** How far the page's own clock is advanced while a confirmation sits armed.
 *
 * Ten times the deleted six-second limit, and comfortably past two full
 * `SAFETY_POLL_INTERVAL_MS` cycles (25s), so one number serves both the
 * criterion pin and the rebuild pin. The point is not that sixty seconds is the
 * requirement — no number is — but that a reader's own pace is not the
 * dashboard's business. It costs nothing: the clock is frozen, so this is
 * arithmetic rather than waiting. */
const ARMED_HORIZON_MS = 60_000;

/** Direct-to-daemon status writes, so the fixtures this file needs exist
 * before the page is opened and are gone after it, whatever the assertions did.
 * The UI is the subject of the test; setup and teardown have no business going
 * through it.
 *
 * `X-Storyhook` is not decoration and not the token: it is what
 * `mutation_guard_ok` (`src/api/http.rs`) requires of every mutating request, a
 * header a cross-origin form post cannot set. Omitting it answers 403 before
 * the token is consulted. */
async function statusApi(
  request: APIRequestContext,
  method: "get" | "post" | "delete",
  path: string,
  data?: Record<string, unknown>,
): Promise<{ status: number; body: string }> {
  const resp = await request[method](path, {
    headers: {
      "X-Storyhook": "1",
      "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
    },
    ...(method === "get" ? {} : { data: data ?? {} }),
  });
  // The body travels with the status deliberately: the daemon says *why* it
  // refused, and a setup failure that reports only "expected 201, got 422"
  // costs a round of guessing at a suite that takes a minute to run.
  return { status: resp.status(), body: await resp.text() };
}

function statesPath(project: string): string {
  return `/api/repos/${encodeURIComponent(project)}/states`;
}

async function addScratchStatus(
  request: APIRequestContext,
  project: string,
  slug: string,
): Promise<void> {
  const { status, body } = await statusApi(request, "post", statesPath(project), {
    slug,
    super_state: "OPEN",
  });
  expect(
    `${status} ${body}`,
    `seeding ${slug} into ${SCRATCH_PROJECT} failed — the tests below would ` +
      "then pass vacuously against a row that was never there",
  ).toMatch(/^201 /);
}

/** Removes a scratch status if it survived the test. Tolerates the 404 a
 * successful deletion leaves behind: "already gone" is the expected outcome of
 * half these tests, not an error. */
async function removeScratchStatus(
  request: APIRequestContext,
  project: string,
  slug: string,
): Promise<void> {
  await statusApi(
    request,
    "delete",
    `${statesPath(project)}/${encodeURIComponent(slug)}`,
  );
}

/** Whether the daemon still holds `slug` — asked of the server, not the DOM.
 *
 * A test that only checks the row is gone from the page cannot tell a deletion
 * from a render bug, and a test that only checks the row is still present
 * cannot tell a withdrawn confirmation from a failed request. */
async function statusExists(
  request: APIRequestContext,
  project: string,
  slug: string,
): Promise<boolean> {
  const { body } = await statusApi(request, "get", statesPath(project));
  const states: Array<{ slug: string }> = JSON.parse(body).states ?? [];
  return states.some((s) => s.slug === slug);
}

/** Opens the statuses editor for `project` from a cold page load. */
async function openStatuses(page: Page, project: string): Promise<void> {
  await page.locator("#settings-btn").click();
  await expect(page.locator("#settings-view")).toBeVisible();
  await page
    .locator(".settings-table tbody tr", { hasText: project })
    .getByRole("button", { name: "Statuses" })
    .click();
  await expect(page.locator(".settings-head h2")).toHaveText(
    `Statuses · ${project}`,
  );
}

const row = (page: Page, slug: string): Locator =>
  page.locator(".status-row", { hasText: slug });

/** The row's own trigger — exact, because when the row is armed its
 * confirmation carries a second button whose name *starts* with "Delete". */
const trigger = (page: Page, slug: string): Locator =>
  row(page, slug).getByRole("button", { name: "Delete", exact: true });

/** The destructive control, which exists only while the row is armed and names
 * the status it would destroy. That the two have different accessible names is
 * itself part of the fix: it is what lets a screen-reader user reading a list
 * of buttons tell the confirm from the trigger. */
const confirm = (page: Page, slug: string): Locator =>
  row(page, slug).getByRole("button", { name: `Delete ${slug}`, exact: true });

const cancel = (page: Page, slug: string): Locator =>
  row(page, slug).getByRole("button", { name: "Cancel", exact: true });

async function expectArmed(page: Page, slug: string): Promise<void> {
  await expect(row(page, slug).locator(".status-confirm")).toHaveCount(1);
  await expect(confirm(page, slug)).toBeVisible();
  await expect(cancel(page, slug)).toBeVisible();
}

async function expectNotArmed(page: Page, slug: string): Promise<void> {
  await expect(row(page, slug).locator(".status-confirm")).toHaveCount(0);
  await expect(trigger(page, slug)).toBeVisible();
}

test.beforeEach(async ({ page, request }) => {
  await seedToken(page);
  const project = await projectSlug(request, SCRATCH_PROJECT);
  await addScratchStatus(request, project, SCRATCH_A);
  await addScratchStatus(request, project, SCRATCH_B);
});

test.afterEach(async ({ request }) => {
  const project = await projectSlug(request, SCRATCH_PROJECT);
  await removeScratchStatus(request, project, SCRATCH_A);
  await removeScratchStatus(request, project, SCRATCH_B);
});

/** Opens the editor with the page's timers faked but still running.
 * `install()` must precede navigation and freezes nothing on its own —
 * freezing is `onAFrozenClock`'s job, around the behaviour under test. */
async function openClockedEditor(page: Page): Promise<void> {
  await page.clock.install();
  await page.goto("/");
  await openStatuses(page, SCRATCH_PROJECT);
}

test("an armed delete confirmation can still be completed a minute later", async ({
  page,
  request,
}) => {
  await openClockedEditor(page);
  await expect(row(page, SCRATCH_A)).toHaveCount(1);

  await onAFrozenClock(page, async () => {
    await trigger(page, SCRATCH_A).click();
    await expectArmed(page, SCRATCH_A);
    // A minute of the page's own time, spent by a reader who was interrupted.
    // Nothing here is a wall-clock wait.
    await page.clock.runFor(ARMED_HORIZON_MS);
    await expectArmed(page, SCRATCH_A);
  });

  // The second half of the gesture the user began, arriving late. It has to
  // complete the deletion — a click that silently re-armed instead was the
  // defect, and was indistinguishable to the user from one that worked.
  await confirm(page, SCRATCH_A).click();
  await expect(row(page, SCRATCH_A)).toHaveCount(0);
  expect(
    await statusExists(request, await projectSlug(request, SCRATCH_PROJECT), SCRATCH_A),
  ).toBe(false);
});

test("an armed confirmation survives a refresh nobody asked for", async ({
  page,
}) => {
  await openClockedEditor(page);
  await trigger(page, SCRATCH_A).click();
  await expectArmed(page, SCRATCH_A);

  // Reproduces on Blink the state WebKit hands the page for free: there, a
  // click does not focus a `<button>`, so `document.activeElement` is `BODY`
  // the instant the user arms, `statusEditorIsBusy()` reads false, and the
  // safety poll rebuilds the rows underneath them.
  //
  // THIS IS A SIMULATION, NOT A SAFARI TEST. This suite installs chromium
  // only (see the file header). What it proves is that the armed state does
  // not depend on focus — which is the property the fix is built on — not
  // that Safari behaves.
  await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
  await expect
    .poll(() => page.evaluate(() => document.activeElement?.tagName))
    .toBe("BODY");

  await onAFrozenClock(page, async () => {
    // Past two full safety-poll cycles, so the rebuild this is about has
    // certainly happened rather than probably.
    await page.clock.runFor(ARMED_HORIZON_MS);
  });

  await expectArmed(page, SCRATCH_A);
  await confirm(page, SCRATCH_A).click();
  await expect(row(page, SCRATCH_A)).toHaveCount(0);
});

test("arming a second row withdraws the first", async ({ page }) => {
  await openClockedEditor(page);

  await trigger(page, SCRATCH_A).click();
  await expectArmed(page, SCRATCH_A);
  await trigger(page, SCRATCH_B).click();

  await expectArmed(page, SCRATCH_B);
  await expectNotArmed(page, SCRATCH_A);
  // The property stated as the invariant it is: one slug cannot name two rows,
  // so "every empty status left armed at once" is unrepresentable rather than
  // merely unlikely. The old design had no such rule — it could arm all of
  // them, and relied on a clock to tidy up after itself.
  await expect(page.locator(".status-confirm")).toHaveCount(1);
});

test("Cancel withdraws the confirmation and keeps the status", async ({
  page,
  request,
}) => {
  await openClockedEditor(page);
  await trigger(page, SCRATCH_A).click();
  await expectArmed(page, SCRATCH_A);

  await cancel(page, SCRATCH_A).click();

  await expectNotArmed(page, SCRATCH_A);
  // Cancel is what the six-second clock was secretly providing: before this
  // change, waiting was the ONLY way to stand down a confirmation armed by
  // mistake. Deleting the timer without adding this would have left a primed
  // destructive control with no way to un-prime it.
  expect(
    await statusExists(request, await projectSlug(request, SCRATCH_PROJECT), SCRATCH_A),
  ).toBe(true);
  // Focus returns to the control the user pressed, rather than being dropped
  // at the top of the document by the re-render.
  await expect(trigger(page, SCRATCH_A)).toBeFocused();
});

test("Escape withdraws the confirmation", async ({ page, request }) => {
  await openClockedEditor(page);
  await trigger(page, SCRATCH_A).click();
  await expectArmed(page, SCRATCH_A);

  await page.keyboard.press("Escape");

  await expectNotArmed(page, SCRATCH_A);
  expect(
    await statusExists(request, await projectSlug(request, SCRATCH_PROJECT), SCRATCH_A),
  ).toBe(true);
});

test("a double-click on Delete cannot delete", async ({ page, request }) => {
  await openClockedEditor(page);

  // The hazard the old design had and the clock never guarded: it armed on one
  // `click` and confirmed on the next at the SAME COORDINATES, so a
  // double-click — the commonest unintended input, and what a user with tremor
  // or spasticity produces without meaning to — armed and confirmed in a single
  // gesture. Six seconds is exactly the wrong shape to stop that.
  //
  // What stops it now is layout, not timing: the confirm is on its own line
  // beneath the row, and what sits at the trigger's coordinates once armed is
  // Cancel. So the second click of a double-click withdraws.
  await trigger(page, SCRATCH_A).dblclick();

  await expectNotArmed(page, SCRATCH_A);
  expect(
    await statusExists(request, await projectSlug(request, SCRATCH_PROJECT), SCRATCH_A),
  ).toBe(true);
});

test("editing another row withdraws an armed confirmation", async ({
  page,
  request,
}) => {
  await openClockedEditor(page);
  await trigger(page, SCRATCH_A).click();
  await expectArmed(page, SCRATCH_A);

  // A mutation on a different row. `statusMutation()`'s callbacks redraw from
  // the server's authoritative list, and cannot be gated on a busy predicate
  // without leaving the editor showing stale counts — so the confirmation is
  // withdrawn there by an explicit rule rather than eaten by a rebuild.
  //
  // Pinned so a later editor cannot quietly widen that rule from "the list
  // changed" back into "any render", which is where the defect came from. The
  // distinction is the whole conformance claim: this withdrawal is triggered by
  // the user's own edit, never by elapsed time.
  await row(page, SCRATCH_B)
    .getByLabel(`${SCRATCH_B} description`)
    .fill("edited by SH-324's spec");
  await page.keyboard.press("Tab");

  await expectNotArmed(page, SCRATCH_A);
  expect(
    await statusExists(request, await projectSlug(request, SCRATCH_PROJECT), SCRATCH_A),
  ).toBe(true);
});
