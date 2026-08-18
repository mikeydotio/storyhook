import { expect, test } from "./support";
import type { APIRequestContext, Locator, Page } from "@playwright/test";
import {
  cleanUpCreatedStories,
  onAFrozenClock,
  projectSlug,
  requiredEnv,
  seedToken,
} from "./support";

/**
 * The statuses editor's "move these stories to …" question (SH-334) — the
 * sibling `status-delete-confirm.spec.ts` (SH-324) filed rather than folded
 * in, because it is a second behaviour change with its own state model: a
 * select value plus a pending intent, not one slug.
 *
 * `promptForDestination` appended its panel straight into a live `.status-row`
 * node and held the editor's refreshes off with nothing but `select.focus()`
 * — the exact mechanism SH-324's own council rejected for the sibling
 * confirmation, and documented as unsound: WebKit does not focus a `<button>`
 * on click, and `statusMutation()`'s two callbacks consult no busy guard at
 * all. The moment focus left the panel for any reason — a click on the page
 * background, a Tab away — the 25s safety poll or the next `/api/events` push
 * silently discarded a destination the user had chosen but not applied. WCAG
 * 2.2 SC 2.2.1's central case, a limit on *completing an action*, and
 * undeclared: no constant, no `setTimeout` — the expiry site was a render.
 *
 * The fix is SH-324's own shape, extended: `state.statusPrompt` (unified with
 * `armedDeleteSlug`, council verdict on SH-334) is painted by
 * `buildStatusRow()` on every render. Two things follow, and both get their
 * own tests below rather than being assumed from SH-324's coverage:
 *
 * 1. **Surviving the QUESTION is not the same fact as surviving the ANSWER.**
 *    SH-324 only ever had one bit to preserve (armed or not). This prompt
 *    carries a chosen destination and, for a reclassify, a pending superstate
 *    — both have to repaint from `state` too, or the panel survives while the
 *    user's own choice quietly resets to nothing.
 * 2. **This file runs under both desktop engines now** (SH-335: `chromium` and
 *    `webkit`, keyed off the same selector). The "survives a refresh nobody
 *    asked for" spec's own `page.evaluate(() => ... .blur())` call was
 *    written to reproduce on Blink the state WebKit hands the page for free
 *    -- under the real `webkit` project that call is redundant with the
 *    engine's own behaviour rather than a simulation of it, but asserts the
 *    identical invariant either way, so it stays unconditional rather than
 *    growing an engine branch for no behavioural difference.
 */

/** Two scratch statuses this file creates and destroys, in a project no other
 * spec asserts a column shape for (see `SCRATCH_PROJECT` below).
 *
 * `sh334-occupied` holds the scratch story every test in this file seeds, so
 * `open_count > 0` and the destination question exists at all. `sh334-empty`
 * has none — it is the "arm the sibling SH-324 confirmation" fixture for the
 * mutual-exclusion spec, and the "destination that vanishes mid-question"
 * fixture for the race spec, freshly re-seeded per test so a deletion in one
 * test never starves the next.
 *
 * Delta Project's own default statuses (`todo`, `in-progress`, `blocked`,
 * `done` — every project's undeletable canonical set) supply every OTHER
 * destination these tests need; only the one status that a test itself
 * deletes is ever a scratch one, so no test here can corrupt a fixture
 * `status-delete-confirm.spec.ts` or any other file depends on. */
const SCRATCH_OCCUPIED = "sh334-occupied";
const SCRATCH_EMPTY = "sh334-empty";

/** The project the scratch statuses and story are minted in — Delta, for the
 * same reason `status-delete-confirm.spec.ts` uses it: it has a checkout
 * (writes to a checkout-less project answer 422), and nothing else asserts a
 * column shape for it byte-for-byte the way Alpha's fixture does. */
const SCRATCH_PROJECT = "Delta Project";

/** Ten times SH-324's own six-second clock, and comfortably past two full
 * `SAFETY_POLL_INTERVAL_MS` cycles (25s) — one number serves the criterion pin
 * and the rebuild pin, the same choice `status-delete-confirm.spec.ts` makes.
 * The clock is frozen, so this is arithmetic, not waiting. */
const ARMED_HORIZON_MS = 60_000;

const AUTH_HEADERS = {
  "X-Storyhook": "1",
  "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
};

async function statusApi(
  request: APIRequestContext,
  method: "get" | "post" | "delete",
  path: string,
  data?: Record<string, unknown>,
): Promise<{ status: number; body: string }> {
  const resp = await request[method](path, {
    headers: AUTH_HEADERS,
    ...(method === "get" ? {} : { data: data ?? {} }),
  });
  return { status: resp.status(), body: await resp.text() };
}

function statesPath(project: string): string {
  return `/api/repos/${encodeURIComponent(project)}/states`;
}

function storyPath(project: string, id: string): string {
  return `/api/repos/${encodeURIComponent(project)}/story/${encodeURIComponent(id)}`;
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

/** Removes a scratch status, sending any stories it still holds to a real
 * fixture status first — teardown order relative to `cleanUpCreatedStories`'s
 * own `afterEach` (registered before this file's, so it runs AFTER this one,
 * Playwright's reverse-registration order) must not matter, and passing a
 * destination is what makes that true: the story lands in `blocked` and is
 * then swept there instead of failing this call with "status is occupied".
 * Tolerates 404 -- "already gone" is the expected outcome of half these
 * tests, not an error. */
async function removeScratchStatus(
  request: APIRequestContext,
  project: string,
  slug: string,
): Promise<void> {
  await statusApi(request, "delete", `${statesPath(project)}/${encodeURIComponent(slug)}`, {
    move_stories_to: "blocked",
  });
}

async function statusExists(
  request: APIRequestContext,
  project: string,
  slug: string,
): Promise<boolean> {
  const { body } = await statusApi(request, "get", statesPath(project));
  const states: Array<{ slug: string }> = JSON.parse(body).states ?? [];
  return states.some((s) => s.slug === slug);
}

async function createScratchStory(
  request: APIRequestContext,
  project: string,
  title: string,
): Promise<string> {
  const resp = await request.post(
    `/api/repos/${encodeURIComponent(project)}/story`,
    { headers: AUTH_HEADERS, data: { title, state: SCRATCH_OCCUPIED } },
  );
  expect(
    resp.status(),
    `seeding a scratch story into ${SCRATCH_OCCUPIED} failed — every test below needs ` +
      "open_count > 0 on that status to have a question to open at all",
  ).toBe(201);
  const payload = await resp.json();
  return payload.story.story.id as string;
}

/** The story's current status slug and superstate, read from the server —
 * never from the DOM, which cannot tell a real move from a render bug. */
async function storyLocation(
  request: APIRequestContext,
  project: string,
  id: string,
): Promise<{ state: string; superstate: string }> {
  const resp = await request.get(storyPath(project, id), { headers: AUTH_HEADERS });
  expect(resp.ok(), `GET ${id} failed: ${await resp.text()}`).toBe(true);
  const payload = await resp.json();
  return { state: payload.story.story.state, superstate: payload.story.story.superstate };
}

async function openStatuses(page: Page, project: string): Promise<void> {
  await page.locator("#settings-btn").click();
  await expect(page.locator("#settings-view")).toBeVisible();
  await page
    .locator(".settings-table tbody tr", { hasText: project })
    .getByRole("button", { name: "Statuses" })
    .click();
  await expect(page.locator(".settings-head h2")).toHaveText(`Statuses · ${project}`);
}

async function openClockedEditor(page: Page): Promise<void> {
  await page.clock.install();
  await page.goto("/");
  await openStatuses(page, SCRATCH_PROJECT);
}

const row = (page: Page, slug: string): Locator =>
  page.locator(".status-row", { hasText: slug });

/** The row's own trigger, before any question is open on it. Exact, because
 * an open question's destructive control (SH-324's confirm) has a name that
 * *starts* with "Delete". */
const trigger = (page: Page, slug: string): Locator =>
  row(page, slug).getByRole("button", { name: "Delete", exact: true });

/** The row's trigger once ANY question is open on it — uniformly "Cancel"
 * regardless of intent (council verdict, question 2): the destination panel
 * carries no Cancel of its own, mirroring SH-324's confirm panel, so this is
 * the only Cancel in the row. */
const cancelTrigger = (page: Page, slug: string): Locator =>
  row(page, slug).getByRole("button", { name: "Cancel", exact: true });

const destinationPanel = (page: Page, slug: string): Locator =>
  row(page, slug).locator(".status-destination");

const destinationSelect = (page: Page, slug: string): Locator =>
  destinationPanel(page, slug).locator("select");

const applyButton = (page: Page, slug: string): Locator =>
  destinationPanel(page, slug).getByRole("button", { name: "Apply", exact: true });

const superSelect = (page: Page, slug: string): Locator =>
  row(page, slug).getByLabel(`${slug} superstate`);

async function expectNoQuestionOpen(page: Page, slug: string): Promise<void> {
  await expect(destinationPanel(page, slug)).toHaveCount(0);
  await expect(row(page, slug).locator(".status-confirm")).toHaveCount(0);
  await expect(trigger(page, slug)).toBeVisible();
}

cleanUpCreatedStories(SCRATCH_PROJECT);

let scratchStoryId: string;

test.beforeEach(async ({ page, request }) => {
  await seedToken(page);
  const project = await projectSlug(request, SCRATCH_PROJECT);
  await addScratchStatus(request, project, SCRATCH_OCCUPIED);
  await addScratchStatus(request, project, SCRATCH_EMPTY);
  scratchStoryId = await createScratchStory(request, project, "SH-334 e2e fixture");
});

test.afterEach(async ({ request }) => {
  const project = await projectSlug(request, SCRATCH_PROJECT);
  await removeScratchStatus(request, project, SCRATCH_OCCUPIED);
  await removeScratchStatus(request, project, SCRATCH_EMPTY);
});

test("a destination question can still be applied a minute later", async ({
  page,
  request,
}) => {
  await openClockedEditor(page);

  await onAFrozenClock(page, async () => {
    await trigger(page, SCRATCH_OCCUPIED).click();
    await expect(destinationPanel(page, SCRATCH_OCCUPIED)).toBeVisible();
    await page.clock.runFor(ARMED_HORIZON_MS);
    await expect(destinationPanel(page, SCRATCH_OCCUPIED)).toBeVisible();
  });

  await destinationSelect(page, SCRATCH_OCCUPIED).selectOption("blocked");
  await applyButton(page, SCRATCH_OCCUPIED).click();

  await expect(row(page, SCRATCH_OCCUPIED)).toHaveCount(0);
  const project = await projectSlug(request, SCRATCH_PROJECT);
  expect(await statusExists(request, project, SCRATCH_OCCUPIED)).toBe(false);
  expect((await storyLocation(request, project, scratchStoryId)).state).toBe("blocked");
});

test("a destination question survives a refresh nobody asked for", async ({ page }) => {
  await openClockedEditor(page);
  await trigger(page, SCRATCH_OCCUPIED).click();
  await expect(destinationPanel(page, SCRATCH_OCCUPIED)).toBeVisible();

  // Reproduces on Blink the state WebKit hands the page for free: there, a
  // click does not focus a `<button>`, so `document.activeElement` stays
  // `BODY` the instant the user opens the question. Under the real `webkit`
  // project (SH-335) this is redundant with the engine's own behaviour, not
  // a simulation of it -- kept unconditional since the assertion is the same
  // either way.
  await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
  await expect
    .poll(() => page.evaluate(() => document.activeElement?.tagName))
    .toBe("BODY");

  await onAFrozenClock(page, async () => {
    await page.clock.runFor(ARMED_HORIZON_MS);
  });

  await expect(destinationPanel(page, SCRATCH_OCCUPIED)).toBeVisible();
  await destinationSelect(page, SCRATCH_OCCUPIED).selectOption("blocked");
  await applyButton(page, SCRATCH_OCCUPIED).click();
  await expect(row(page, SCRATCH_OCCUPIED)).toHaveCount(0);
});

test("a chosen destination survives a repaint, not just the question", async ({
  page,
  request,
}) => {
  await openClockedEditor(page);
  await trigger(page, SCRATCH_OCCUPIED).click();

  // A non-default pick: the placeholder occupies the first position, so any
  // real choice already proves the select isn't merely showing its own
  // default -- but "in-progress" is picked explicitly rather than assumed
  // second, since Delta's status vocabulary is a fixture that may grow.
  await destinationSelect(page, SCRATCH_OCCUPIED).selectOption("in-progress");
  await expect(destinationSelect(page, SCRATCH_OCCUPIED)).toHaveValue("in-progress");

  await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
  await onAFrozenClock(page, async () => {
    await page.clock.runFor(ARMED_HORIZON_MS);
  });

  // The property SH-324 has no analogue for: the QUESTION surviving a repaint
  // says nothing about whether the ANSWER did. A build that repaints the
  // panel but resets the select to its placeholder passes a "the panel is
  // there" check and fails this one.
  await expect(destinationSelect(page, SCRATCH_OCCUPIED)).toHaveValue("in-progress");

  await applyButton(page, SCRATCH_OCCUPIED).click();
  const project = await projectSlug(request, SCRATCH_PROJECT);
  expect((await storyLocation(request, project, scratchStoryId)).state).toBe("in-progress");
});

test("a pending superstate change survives a repaint too", async ({ page, request }) => {
  await openClockedEditor(page);

  await superSelect(page, SCRATCH_OCCUPIED).selectOption("CLOSED");
  await expect(destinationPanel(page, SCRATCH_OCCUPIED)).toContainText(
    `Moving 1 story out of ${SCRATCH_OCCUPIED} first`,
  );
  // The row's own select shows the PENDING value, not the server's -- painting
  // status.super_state here instead would leave the control and the sentence
  // beneath it disagreeing from the very first repaint, with no second render
  // to notice.
  await expect(superSelect(page, SCRATCH_OCCUPIED)).toHaveValue("CLOSED");

  await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
  await onAFrozenClock(page, async () => {
    await page.clock.runFor(ARMED_HORIZON_MS);
  });

  await expect(destinationPanel(page, SCRATCH_OCCUPIED)).toBeVisible();
  await expect(superSelect(page, SCRATCH_OCCUPIED)).toHaveValue("CLOSED");

  await destinationSelect(page, SCRATCH_OCCUPIED).selectOption("done");
  await applyButton(page, SCRATCH_OCCUPIED).click();

  const project = await projectSlug(request, SCRATCH_PROJECT);
  const location = await storyLocation(request, project, scratchStoryId);
  expect(location.state).toBe("done");
  expect(location.superstate).toBe("CLOSED");
});

test("one question is open in the editor at a time", async ({ page }) => {
  await openClockedEditor(page);

  await trigger(page, SCRATCH_OCCUPIED).click();
  await expect(destinationPanel(page, SCRATCH_OCCUPIED)).toBeVisible();

  // Arming SH-324's own empty-status confirmation on a different row replaces
  // the destination question -- one `state.statusPrompt`, not two fields
  // cross-clearing each other (council verdict, question 1).
  await trigger(page, SCRATCH_EMPTY).click();
  await expect(row(page, SCRATCH_EMPTY).locator(".status-confirm")).toBeVisible();
  await expectNoQuestionOpen(page, SCRATCH_OCCUPIED);

  // And the reverse: opening the destination question again replaces the
  // armed confirmation.
  await trigger(page, SCRATCH_OCCUPIED).click();
  await expect(destinationPanel(page, SCRATCH_OCCUPIED)).toBeVisible();
  await expect(row(page, SCRATCH_EMPTY).locator(".status-confirm")).toHaveCount(0);
  await expect(trigger(page, SCRATCH_EMPTY)).toBeVisible();
});

test("Cancel withdraws the destination question and keeps the story put", async ({
  page,
  request,
}) => {
  await openClockedEditor(page);
  await trigger(page, SCRATCH_OCCUPIED).click();
  await destinationSelect(page, SCRATCH_OCCUPIED).selectOption("blocked");

  await cancelTrigger(page, SCRATCH_OCCUPIED).click();

  await expectNoQuestionOpen(page, SCRATCH_OCCUPIED);
  const project = await projectSlug(request, SCRATCH_PROJECT);
  expect((await storyLocation(request, project, scratchStoryId)).state).toBe(SCRATCH_OCCUPIED);
  await expect(trigger(page, SCRATCH_OCCUPIED)).toBeFocused();
});

test("reselecting the original superstate cancels the reclassify question", async ({
  page,
  request,
}) => {
  await openClockedEditor(page);
  await superSelect(page, SCRATCH_OCCUPIED).selectOption("CLOSED");
  await expect(destinationPanel(page, SCRATCH_OCCUPIED)).toBeVisible();

  // Cancelling with the control that opened the question, not a button
  // elsewhere -- the reclassify question carries no Cancel of its own inside
  // the panel (council verdict, question 2), so this select IS the exit.
  await superSelect(page, SCRATCH_OCCUPIED).selectOption("OPEN");

  await expectNoQuestionOpen(page, SCRATCH_OCCUPIED);
  await expect(superSelect(page, SCRATCH_OCCUPIED)).toHaveValue("OPEN");
  await expect(superSelect(page, SCRATCH_OCCUPIED)).toBeFocused();
  const project = await projectSlug(request, SCRATCH_PROJECT);
  const location = await storyLocation(request, project, scratchStoryId);
  expect(location.state).toBe(SCRATCH_OCCUPIED);
  expect(location.superstate).toBe("OPEN");
});

test("Escape withdraws the destination question", async ({ page, request }) => {
  await openClockedEditor(page);
  await trigger(page, SCRATCH_OCCUPIED).click();
  await expect(destinationPanel(page, SCRATCH_OCCUPIED)).toBeVisible();

  await page.keyboard.press("Escape");

  await expectNoQuestionOpen(page, SCRATCH_OCCUPIED);
  const project = await projectSlug(request, SCRATCH_PROJECT);
  expect((await storyLocation(request, project, scratchStoryId)).state).toBe(SCRATCH_OCCUPIED);
});

test("editing another row withdraws an open destination question", async ({
  page,
  request,
}) => {
  await openClockedEditor(page);
  await trigger(page, SCRATCH_OCCUPIED).click();
  await expect(destinationPanel(page, SCRATCH_OCCUPIED)).toBeVisible();

  // A mutation on a DIFFERENT row. `statusMutation()`'s callbacks redraw from
  // the server's authoritative list and consult no busy guard -- withdrawal
  // here is the SH-324 rule, pinned so a later editor cannot quietly widen it
  // from "the list changed" back into "any render", which is where the
  // original defect came from.
  await row(page, SCRATCH_EMPTY).getByLabel(`${SCRATCH_EMPTY} description`).fill("SH-334 spec");
  await page.keyboard.press("Tab");

  await expectNoQuestionOpen(page, SCRATCH_OCCUPIED);
  const project = await projectSlug(request, SCRATCH_PROJECT);
  expect((await storyLocation(request, project, scratchStoryId)).state).toBe(SCRATCH_OCCUPIED);
});

test("Apply refuses until a destination is chosen", async ({ page, request }) => {
  await openClockedEditor(page);
  await trigger(page, SCRATCH_OCCUPIED).click();
  await expect(destinationSelect(page, SCRATCH_OCCUPIED)).toHaveValue("");

  await applyButton(page, SCRATCH_OCCUPIED).click();

  // Refused, not withdrawn and not applied to whatever the browser's own
  // first-option default would have been -- "nothing chosen" is a
  // representable state (council verdict, question 3), and Apply against it
  // says so rather than guessing on the user's behalf.
  await expect(destinationPanel(page, SCRATCH_OCCUPIED)).toBeVisible();
  await expect(destinationSelect(page, SCRATCH_OCCUPIED)).toBeFocused();
  await expect(page.locator("#notice-dock-status")).toHaveText("Choose a destination first.");
  const project = await projectSlug(request, SCRATCH_PROJECT);
  expect((await storyLocation(request, project, scratchStoryId)).state).toBe(SCRATCH_OCCUPIED);
});

test("a destination that vanishes mid-question is cleared, not withdrawn", async ({
  page,
  request,
}) => {
  await openClockedEditor(page);
  await trigger(page, SCRATCH_OCCUPIED).click();
  await destinationSelect(page, SCRATCH_OCCUPIED).selectOption(SCRATCH_EMPTY);
  await expect(destinationSelect(page, SCRATCH_OCCUPIED)).toHaveValue(SCRATCH_EMPTY);

  // The chosen destination is removed out from under the open question --
  // another client, or the CLI. `sh334-empty` holds no stories, so this needs
  // no `move_stories_to` of its own.
  const project = await projectSlug(request, SCRATCH_PROJECT);
  const deleted = await request.delete(
    `${statesPath(project)}/${encodeURIComponent(SCRATCH_EMPTY)}`,
    { headers: AUTH_HEADERS, data: {} },
  );
  expect(deleted.ok(), `deleting ${SCRATCH_EMPTY} failed: ${await deleted.text()}`).toBe(true);

  await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
  await onAFrozenClock(page, async () => {
    await page.clock.runFor(ARMED_HORIZON_MS);
  });

  // Cleared to "nothing chosen", not silently re-pointed at another status
  // and not withdrawn along with the user's place in the row (council
  // verdict, question 3) -- the question survives, the stale answer does not.
  await expect(destinationPanel(page, SCRATCH_OCCUPIED)).toBeVisible();
  await expect(destinationSelect(page, SCRATCH_OCCUPIED)).toHaveValue("");
  await expect(destinationSelect(page, SCRATCH_OCCUPIED).getByRole("option", {
    name: SCRATCH_EMPTY,
  })).toHaveCount(0);

  await destinationSelect(page, SCRATCH_OCCUPIED).selectOption("blocked");
  await applyButton(page, SCRATCH_OCCUPIED).click();
  expect((await storyLocation(request, project, scratchStoryId)).state).toBe("blocked");
});
