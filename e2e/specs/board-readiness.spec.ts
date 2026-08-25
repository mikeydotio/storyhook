import { test, expect } from "./support";
import type { APIRequestContext } from "@playwright/test";
import {
  activateBehindOverlay,
  cleanUpCreatedStories,
  heldReadDeadlineMs,
  holdUntilRefused,
  latch,
  openProject,
  projectSlug,
  requiredEnv,
  seedToken,
} from "./support";

/**
 * Pins SH-222's readiness rule: reaching a board means its data has arrived,
 * not that the board is on screen.
 *
 * Both tests run with `/data` deliberately slowed. That delay is the only
 * thing a busy machine was ever contributing: `selectRepo()` renders the
 * board screen from `state.data = null` and fetches afterwards, so the
 * window between "board visible" and "board has data" exists on every run —
 * it is simply too narrow to lose on an idle machine, and wide enough to
 * lose after a fifteen-minute gate. Reproducing it with a route delay rather
 * than with load is what makes it a test instead of an anecdote.
 */

const DATA_DELAY_MS = 2000;

test("choosing Settings while the initial catalog is in flight is not undone by bootstrap", async ({
  page,
}) => {
  await seedToken(page);
  const catalogGate = latch();
  const catalogRequested = latch();
  await page.route(/\/api\/repos$/, async (route) => {
    if (route.request().method() !== "GET") {
      await route.continue();
      return;
    }
    catalogRequested.release();
    await catalogGate.held;
    await route.continue();
  });

  await page.goto("/");
  await catalogRequested.held;
  await page.locator("#settings-btn").click();
  await expect(page.locator("#settings-view")).toBeVisible();

  // Both bootstrap's first read and goSettings()'s refresh may be waiting on
  // this gate. Releasing them together constructs the production race: the
  // initial callback lands only after the user's navigation has committed.
  catalogGate.release();
  await expect(
    page.locator(".settings-table tbody tr", { hasText: "Alpha Project" }),
  ).toBeVisible();
  await expect(page.locator("#settings-view")).toBeVisible();
  await expect(page.locator("#home-view")).toBeHidden();
});

/** Holds every project-data read for `DATA_DELAY_MS`. Registered before the
 * navigation that triggers one, since the very first `/data` is the one
 * under test. */
async function slowData(page: import("@playwright/test").Page): Promise<void> {
  await page.route(/\/data(\?|$)/, async (route) => {
    await new Promise((resolve) => setTimeout(resolve, DATA_DELAY_MS));
    await route.continue();
  });
}

/** The title of the draft the SH-284 tests below seed. Distinctive on
 * purpose: `cleanUpCreatedStories` sweeps by id, but a failure message
 * naming this string says immediately which spec left it behind. */
const DRAFT_TITLE = "A draft the pre-data window must not miscount";

/** Seeds one draft into `projectName` through the API, the way SH-284's
 * original reproduction did — the Drafts button's whole subject is a count
 * this project actually has, so every test below needs a real one, and
 * driving the create modal to make it would mean acting inside the very
 * window under test. Swept afterwards by `cleanUpCreatedStories`, whose
 * `/data`-derived baseline already covers drafts as well as board stories.
 *
 * `X-Storyhook` as well as the token: a mutation has to clear
 * `mutation_guard_ok`'s CSRF check, which a read does not.
 */
async function seedDraft(
  request: APIRequestContext,
  projectName: string,
  title: string,
): Promise<void> {
  const slug = await projectSlug(request, projectName);
  const resp = await request.post(
    `/api/repos/${encodeURIComponent(slug)}/story`,
    {
      headers: {
        "X-Storyhook": "1",
        "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
      },
      data: { title, draft: true },
    },
  );
  if (!resp.ok()) {
    throw new Error(
      `seeding a draft in "${projectName}" answered ${resp.status()}: ${await resp.text()}`,
    );
  }
}

cleanUpCreatedStories("Alpha Project");

test("the board is visible, and empty, before its data arrives", async ({
  page,
}) => {
  await seedToken(page);
  await slowData(page);
  // Deep-linked rather than clicked, so this test can observe the window
  // without going through `openProject()` -- the helper whose whole job is
  // to close it.
  const slug = await projectSlug(page.request, "Alpha Project");
  await page.goto(`/?project=${encodeURIComponent(slug)}`);

  await expect(page.locator("#board-view")).toBeVisible();
  // What "visible but not loaded" actually looks like: no cards, and a
  // filter count that renderView() leaves empty because `state.data` is
  // still null. A spec that treated the assertion above as readiness would
  // be acting on this.
  await expect(page.locator(".card")).toHaveCount(0);
  await expect(page.locator("#filter-count")).toHaveText("");

  // And it does resolve on its own -- the window is a window, not a
  // breakage.
  await expect(page.locator("#filter-count")).not.toHaveText("", {
    timeout: DATA_DELAY_MS * 3,
  });
});

test("openProject waits for the data, so the create modal is fully built", async ({
  page,
}) => {
  await seedToken(page);
  await slowData(page);
  await page.goto("/");

  const started = Date.now();
  await openProject(page, "Alpha Project");
  const waited = Date.now() - started;

  // A lower bound on waiting, which is the safe direction to assert in: load
  // can only make this number larger (SH-238's flake was an upper-bound
  // margin, and this is deliberately not one). Below it, `openProject` would
  // have returned on the board screen alone, which is the defect.
  expect(waited).toBeGreaterThan(DATA_DELAY_MS * 0.75);

  // The consequence, which is the reason any of this matters. The create
  // modal is built once, synchronously, from `meta()` at the moment it
  // opens: opened in the window above it holds nothing but its placeholder
  // options and never repopulates, so this line spins out the full test
  // timeout with "did not find some options" rather than failing fast.
  //
  // SH-265 also disabled `#new-story-btn` for that same window, so by the
  // time `openProject()` returns the click below is acting on a button that
  // was already live -- this is what proves it, alongside the third test in
  // this file, which asserts the disabled state directly.
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-priority").selectOption("critical");
  await expect(page.locator("#create-priority")).toHaveValue("critical");

  // Nothing is created: Escape does not dismiss this modal (draft-stories
  // .spec.ts pins that), so the draft is discarded explicitly.
  await page.locator("#create-discard").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
});

test("the + New button is inert until the project's data arrives", async ({
  page,
}) => {
  await seedToken(page);
  await slowData(page);
  // Deep-linked, same as the first test above, so this can observe the
  // pre-data window directly rather than going through `openProject()`.
  const slug = await projectSlug(page.request, "Alpha Project");
  await page.goto(`/?project=${encodeURIComponent(slug)}`);

  await expect(page.locator("#board-view")).toBeVisible();
  const btn = page.locator("#new-story-btn");
  await expect(btn).toBeVisible();
  // SH-265: the button used to stay clickable through this whole window,
  // opening a modal built from an empty `meta()` that never repopulates.
  await expect(btn).toBeDisabled();

  // A disabled button fires no click event at all -- this is a stronger
  // claim than "Playwright's `.click()` would wait", and it is what proves
  // the button is actually inert rather than merely slow to become
  // clickable.
  await page.evaluate(() => {
    (document.getElementById("new-story-btn") as HTMLButtonElement).click();
  });
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);

  // `openCreateModal()`'s own `if (!state.data) return;` guard is the second
  // line of defense -- exercised here by forcing the one path around the
  // button's `disabled` attribute a real user cannot take.
  await page.evaluate(() => {
    const el = document.getElementById("new-story-btn") as HTMLButtonElement;
    el.disabled = false;
    el.click();
  });
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);

  // And once the data lands, the button is live and the modal it opens
  // holds this project's real vocabulary -- not just placeholders.
  await expect(page.locator("#filter-count")).not.toHaveText("", {
    timeout: DATA_DELAY_MS * 3,
  });
  await expect(btn).toBeEnabled();
  await btn.click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-priority").selectOption("critical");
  await expect(page.locator("#create-priority")).toHaveValue("critical");
  await expect(page.locator("#create-state")).toContainText("review");

  await page.locator("#create-discard").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
});

/**
 * SH-284: the Drafts button in the same window, which the council settled
 * differently from its neighbour on purpose (`story show SH-284` carries the
 * verdict in full, including the dissent).
 *
 * `#new-story-btn` above is *disabled* through this window because the create
 * modal is built once, synchronously, from `meta()` and never repopulates —
 * disabling is the only thing that makes a permanently-wrong destination
 * unreachable. The Drafts popover has no such destination: it renders on open
 * and re-renders on every arrival. So this button stays live, and what is fixed
 * instead is what it *claims* — a count it does not have.
 *
 * Every label assertion here is exact rather than a regex, and that is
 * load-bearing: `"Drafts"` is a suffix of `"1 Drafts"`, so `/Drafts$/` passes
 * in both the neutral and the counted state and would go green against the
 * unfixed code.
 *
 * Not asserted here, deliberately: the label at the narrow breakpoint. Under
 * `.icon-compact-btn` the text span is hidden visually by the `.sr-only`
 * recipe, which preserves `textContent` — that is the whole point of the recipe
 * — so these exact-text assertions already cover the string a screen reader
 * announces at every width. The one mobile spec that measures the topbar
 * measures it on a board, where this button's visibility is unchanged.
 */
test("the global Drafts count does not wait for the open board's data", async ({
  page,
  request,
}) => {
  await seedDraft(request, "Alpha Project", DRAFT_TITLE);
  await seedToken(page);
  await slowData(page);
  // Deep-linked, like the tests above, so the window can be observed rather
  // than waited out by `openProject()`.
  const slug = await projectSlug(page.request, "Alpha Project");
  await page.goto(`/?project=${encodeURIComponent(slug)}`);

  await expect(page.locator("#board-view")).toBeVisible();
  const btn = page.locator("#drafts-btn");
  const label = page.locator("#drafts-btn-text");
  await expect(btn).toBeVisible();
  // The deliberate divergence from `#new-story-btn`, pinned directly: this
  // control stays operable, because it has an honest thing to show.
  await expect(btn).toBeEnabled();
  // The catalog, not Alpha's held board payload, owns this global count.
  await expect(label).toHaveText("1 Drafts");
  await expect(page.locator("#new-story-btn")).toBeDisabled();

  await btn.click();
  await expect(page.locator("#drafts-modal")).toHaveClass(/open/);
  await expect(page.locator("#drafts-list .drafts-row")).toHaveCount(1);
  await expect(page.locator("#drafts-list .drafts-row")).toContainText(
    DRAFT_TITLE,
  );
  await expect(page.locator("#drafts-list .drafts-row-project")).toHaveText(
    "AA · Alpha Project",
  );
  await expect(page.locator("#drafts-modal")).toHaveClass(/open/);
});

test("a board switch does not re-scope the global draft count or rows", async ({
  page,
  request,
}) => {
  await seedDraft(request, "Alpha Project", DRAFT_TITLE);
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await expect(page.locator("#drafts-btn-text")).toHaveText("1 Drafts");

  await page.locator("#drafts-btn").click();
  await expect(page.locator("#drafts-list .drafts-row")).toHaveCount(1);

  // The switch cannot be driven by hand from here, and that is now true of
  // both input devices. `.backdrop` is `position: fixed; inset: 0` over an
  // unpositioned topbar, so a *mouse* click on `#projsel-btn` hits the
  // backdrop and closes the popover instead; this test used to reach it with
  // Enter on the focused selector, which SH-299 closed on purpose by marking
  // the background `inert`. So the two steps below go through
  // `activateBehindOverlay()`, which runs the selector's own listeners
  // without a gesture. See that helper's comment.
  //
  // What that makes this test, since SH-292 re-checked it: a *structural*
  // invariant rather than a reachable scenario. The comment here used to
  // claim the state was still reachable with no gesture at all, citing
  // `fetchReposOnce()`'s `goHome()` on a project deleted elsewhere -- but
  // that path dismisses the popover rather than re-pointing it (the next
  // test asserts exactly that), so no production route reaches a
  // board-to-board switch with this popover open. The assertions below are
  // kept anyway, and are worth keeping: they pin `renderDraftsList()`'s
  // clear-before-branch ordering, which nothing else ties to this decision,
  // and they would be the first thing to fail if a future change reopened
  // the switch. The state SH-292's naming actually exists for is the parked
  // failure below, which is why that test asserts the subject line too.
  await slowData(page);
  await activateBehindOverlay(page.locator("#projsel-btn"));
  await expect(page.locator("#projsel-menu")).toBeVisible();
  await activateBehindOverlay(
    page.locator("#projsel-menu .projsel-item", { hasText: "Beta Project" }),
  );

  // Draft discovery is global: Alpha's draft remains reachable while Beta's
  // board vocabulary is still loading, with its owner stated on the row.
  expect(await page.locator("#drafts-btn-text").textContent()).toBe("1 Drafts");
  await expect(page.locator("#drafts-modal .modal-header")).toHaveText("Drafts");
  await expect(page.locator("#drafts-list .drafts-row")).toHaveCount(1);
  await expect(page.locator("#drafts-list .drafts-row-project")).toHaveText(
    "AA · Alpha Project",
  );
  await expect(page.locator("#drafts-modal")).toHaveClass(/open/);
});

test("the Drafts button and popover are available on every dashboard screen", async ({
  page,
  request,
}) => {
  await seedDraft(request, "Alpha Project", DRAFT_TITLE);
  await seedToken(page);
  await page.goto("/");

  await expect(page.locator("#drafts-btn")).toBeVisible();
  await expect(page.locator("#drafts-btn-text")).toHaveText("1 Drafts");

  await openProject(page, "Alpha Project");
  await expect(page.locator("#drafts-btn")).toBeVisible();
  await expect(page.locator("#drafts-btn-text")).toHaveText("1 Drafts");

  await page.locator("#drafts-btn").click();
  await expect(page.locator("#drafts-modal")).toHaveClass(/open/);
  await expect(page.locator("#drafts-list .drafts-row")).toHaveCount(1);

  // Drive navigation structurally behind the modal: because Drafts is global,
  // neither its owning control nor its rows become orphaned on Home.
  await activateBehindOverlay(page.locator("#home-btn"));

  await expect(page.locator("#drafts-btn")).toBeVisible();
  await expect(page.locator("#drafts-modal")).toHaveClass(/open/);
  await expect(page.locator("#drafts-list .drafts-row")).toHaveCount(1);

  await page.locator("#drafts-close").click();
  await page.locator("#settings-btn").click();
  await expect(page.locator("#drafts-btn")).toBeVisible();
  await expect(page.locator("#drafts-btn-text")).toHaveText("1 Drafts");
});

/**
 * A popover opened just after a navigation must be fully operable — pinning
 * the delayed write in `closeDraftsModal()` against the state it lands in.
 *
 * That function hides the backdrop on a 150ms timer, so the fade-out has
 * something to fade. SH-284 gave it a second caller that runs on *every*
 * navigation to a non-repo screen — including the `renderScreen()` at module
 * init and `bootstrap()`'s own `goHome()` — so on an ordinary page load two
 * such timers are in flight before the user has done anything. Open the
 * popover inside that window and the timer lands on it: the backdrop takes
 * `hidden` while the modal keeps `open`, which is invisible, unclickable, and
 * self-inflicted. `draft-stories.spec.ts`'s outside-click test caught it as a
 * 15s timeout on a backdrop it could see in the DOM and not click.
 *
 * The defect predates this story and was fixed in its own commit at the
 * origin, covering the Escape caller that could always reach it — which is
 * what `draft-stories.spec.ts` pins. This test pins the other half: that the
 * caller *this* story adds, on a path every page load takes, does not walk
 * back into it. The origin fix and the new caller arrived together and can
 * drift apart.
 *
 * That origin fix was a timer that re-read the popover's state when it fired,
 * and SH-302 replaced it: `.open` lands a frame after `hidden` comes off, so
 * a timer inside that frame read an open popover as closed and hid the
 * backdrop anyway. Cancelling the timer on reopen asks no such question, and
 * `overlay-reopen-race.spec.ts` is where that is pinned across the overlays
 * that share the shape. This test is unchanged by it: what it asserts is the
 * navigation caller's consequence, whatever the repair underneath.
 */
test("a popover opened moments after a navigation is not hidden by the previous close", async ({
  page,
  request,
}) => {
  await seedDraft(request, "Alpha Project", DRAFT_TITLE);
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");

  // Home, back into the project, then open the popover -- three real clicks
  // on three real controls, compressed into one JavaScript tick so the race
  // is constructed rather than hoped for. Driven one Playwright action at a
  // time this is ~150-300ms of round trips, which straddles the timer's own
  // 150ms and makes the test pass or fail on machine speed; in one tick the
  // timer is guaranteed to fire after the popover is open, which is the
  // state under test. Every handler invoked here is the one a user's click
  // invokes.
  await page.evaluate(() => {
    (document.getElementById("home-btn") as HTMLElement).click();
    (document.querySelector(".repo-card-name") as HTMLElement).click();
    (document.getElementById("drafts-btn") as HTMLElement).click();
  });

  await expect(page.locator("#drafts-modal")).toHaveClass(/open/);
  // Past the 150ms deliberately. Asserting immediately would pass even
  // against the bug -- the backdrop is genuinely visible until the stale
  // timer fires -- so the assertion has to outlive the timer to mean
  // anything.
  await page.waitForTimeout(250);
  // The invariant, stated where it broke: an open popover's backdrop is
  // never `hidden`. Asserted before the click below because `toBeVisible()`
  // names the actual defect, where a failing click only reports a timeout.
  await expect(page.locator("#drafts-backdrop")).toBeVisible();

  // And it still dismisses the way `draft-stories.spec.ts` says it does.
  await page.locator("#drafts-backdrop").click({ position: { x: 4, y: 4 } });
  await expect(page.locator("#drafts-modal")).not.toHaveClass(/open/);
});

/**
 * SH-291: the same window, when it never ends.
 *
 * Both readiness surfaces above are keyed on `!state.data`, which is also the
 * state a project stays in *permanently* when `/data` times out, errors, or
 * answers 401 — `fetchData()` assigns `state.data` only on a parsed 200. So a
 * store that never answers used to say "Loading…" forever: true, in the sense
 * that the 25s safety poll really is still trying, and useless as an
 * explanation.
 *
 * `state.fetchOk` cannot tell the two apart. It initialises `false`, so it
 * reads identically before the first fetch and after a failed one, and copy
 * keyed on it would name an error during boot, before anything had gone wrong.
 * What the surfaces read instead is `state.fetchSettledFor` — the project whose
 * `/data` has *completed*, with data, with an error, or with no answer at all —
 * and the first test below is red against either shortcut: against the old code
 * on its second assertion, against a `fetchOk` version on its first.
 *
 * The failure is driven by a held request rather than by a timer, unlike
 * `slowData` above. A timer ends its window when the machine gets round to it,
 * which is fine for a spec that only asserts what is on screen *during* the
 * window; these have to assert the transition, so the boundary has to be the
 * test's. It used to not be: the page's own `xhr.timeout = 8000`
 * (`fetchData()`) settled a held request whether or not a test asked it to,
 * so everything asserted before `refuse()` ran on an 8s budget, four times
 * the window the tests above act inside — a real, if load-sensitive, race
 * (SH-347). Each test below now sets `boardFetchTimeoutMs` past anything
 * this harness can ever grant a test (`heldReadDeadlineMs()`, `./support`),
 * so the boundary really is the test's now, not the page's clock racing it.
 */

/** Holds every `/data` read for `slug` in flight until `refuse()` is called.
 *
 * Scoped to one project on purpose: a store is not what fails here, a
 * project's own reads are — which is what lets the third test below hold two
 * projects in two different states at once.
 *
 * A thin wrapper over `holdUntilRefused()` (SH-301, `./support`), which
 * generalised this function beyond one project's `/data` once
 * `catalog-readiness.spec.ts` needed the identical held/refused/restored
 * lifecycle on the catalog and statuses-editor routes. See its own doc
 * comment for why `restore()` flips a flag rather than calling
 * `page.unroute()`. */
function holdDataFor(page: import("@playwright/test").Page, slug: string) {
  return holdUntilRefused(
    page,
    (url) => url.pathname === `/api/repos/${encodeURIComponent(slug)}/data`,
  );
}

test("a board data failure does not poison the global Drafts surface", async ({
  page,
  request,
  browserName,
}) => {
  // Same class as the two `holdDataFor`-based tests below. SH-347's own idle
  // probes (interception-contract.spec.ts, probe B) refuted the original
  // "WebKit doesn't reliably surface an abort" hypothesis -- ~4ms, same as
  // Chromium. But re-measured under a REAL full-suite WebKit run
  // (interception-contract.spec.ts, probe D1, the council-decided plan
  // recorded on this story): a single held route's abort was NOT reflected
  // on screen within 12 seconds of `refuse()`, even though the real
  // navigation/render/click work leading up to it took only ~120ms --
  // against probe D2's ~3ms for the two-route shape in the *same* run.
  // Abort delivery is real but not reliably prompt under contention; this
  // file's own former page-clock race (`xhr.timeout = 8000`, since removed
  // by `boardFetchTimeoutMs`) is not the whole story either. Per the
  // council's own verdict, an unreliable-under-load result keeps the
  // quarantine rather than lifting it on an idle measurement alone.
  test.skip(
    browserName === "webkit",
    "Abort delivery measured NOT reliably prompt under real full-suite WebKit contention (SH-347, council Probe D)",
  );
  await seedDraft(request, "Alpha Project", DRAFT_TITLE);
  await seedToken(page);
  const slug = await projectSlug(page.request, "Alpha Project");
  const alphaData = await holdDataFor(page, slug);
  await page.goto(
    `/?project=${encodeURIComponent(slug)}&boardFetchTimeoutMs=${heldReadDeadlineMs()}`,
  );

  await expect(page.locator("#board-view")).toBeVisible();
  await page.locator("#drafts-btn").click();
  await expect(page.locator("#drafts-modal")).toHaveClass(/open/);

  // The global catalog answered independently of Alpha's held board data.
  const list = page.locator("#drafts-list");
  const newBtn = page.locator("#new-story-btn");
  await expect(list.locator(".drafts-row")).toHaveCount(1);
  await expect(list).toContainText(DRAFT_TITLE);
  await expect(newBtn).toHaveAttribute(
    "title",
    "Loading this project's states, types and labels…",
  );

  await alphaData.refuse();

  // Only the board-scoped create vocabulary reports the board failure.
  await expect(list.locator(".drafts-row")).toHaveCount(1);
  await expect(newBtn).toHaveAttribute(
    "title",
    "Couldn't load this project's states, types and labels. Retrying…",
  );
  // `#new-story-btn` stays disabled, while Drafts keeps its catalog count.
  await expect(newBtn).toBeDisabled();
  await expect(page.locator("#drafts-btn")).toBeEnabled();
  await expect(page.locator("#drafts-btn-text")).toHaveText("1 Drafts");
  await expect(page.locator("#drafts-list .drafts-row-project")).toHaveText(
    "AA · Alpha Project",
  );
});

test("a project that answers after a failure drops the error where it stands", async ({
  page,
  request,
  browserName,
}) => {
  // Observed intermittently in a full-suite run (a fresh isolated run of
  // this test alone passes every time), never reproduced in isolation --
  // and re-measured, not merely re-hypothesized, since. SH-347's own idle
  // probes (interception-contract.spec.ts, probe B) refuted the original
  // "WebKit doesn't reliably surface an abort" hypothesis (~4ms, same as
  // Chromium). But a REAL full-suite WebKit run (probe D1, the
  // council-decided plan recorded on this story) found a single held
  // route's abort NOT reflected on screen within 12s of `refuse()`, against
  // ~3ms for the two-route shape (probe D2) in that same run. Abort
  // delivery is real but not reliably prompt under contention -- this
  // file's own former page-clock race is not the whole explanation either
  // (`boardFetchTimeoutMs` removed it and the flake still isn't proven
  // gone). An unreliable-under-load result keeps the quarantine per the
  // council's own verdict.
  test.skip(
    browserName === "webkit",
    "Abort delivery measured NOT reliably prompt under real full-suite WebKit contention (SH-347, council Probe D)",
  );
  await seedToken(page);
  const slug = await projectSlug(page.request, "Alpha Project");
  const alphaData = await holdDataFor(page, slug);
  await page.goto(
    `/?project=${encodeURIComponent(slug)}&boardFetchTimeoutMs=${heldReadDeadlineMs()}`,
  );

  await expect(page.locator("#board-view")).toBeVisible();
  await page.locator("#drafts-btn").click();
  await alphaData.refuse();
  const list = page.locator("#drafts-list");
  await expect(list).toHaveText("No drafts.");

  // The claim is "Retrying…", so the repair must need no reload, no reopen and
  // no gesture: the store starts answering, and the very next arrival —
  // pushed by the write below over `/api/events`, exactly as another client's
  // would be — replaces the error with the project's real drafts under an
  // already-open popover.
  alphaData.restore();
  await seedDraft(request, "Alpha Project", DRAFT_TITLE);

  await expect(page.locator("#drafts-list .drafts-row")).toHaveCount(1, {
    timeout: 10_000,
  });
  await expect(page.locator("#drafts-list .drafts-row")).toContainText(
    DRAFT_TITLE,
  );
  await expect(page.locator("#drafts-btn-text")).toHaveText("1 Drafts");
});

test("one board's failure never re-scopes the global Drafts surface", async ({
  page,
  browserName,
}) => {
  // Same class as this file's other two `holdDataFor`-based tests,
  // quarantined alongside them. This test holds TWO routes open at once
  // (`holdDataFor` for both Alpha and Beta, only one ever refused). SH-347's
  // own re-measurement under a real full-suite WebKit run (probe D2, the
  // council-decided plan recorded on this story) actually saw THIS shape's
  // abort reflected in ~3ms -- as fast as idle -- in the very same run
  // where the single-route shape (probe D1, this file's other two tests)
  // did not surface within 12s. One green sample for this shape, against a
  // sibling failure in the identical run, does not meet the council's own
  // bar for lifting (N consecutive green legs, stated honestly) -- kept
  // quarantined pending more runs, with the asymmetric result recorded
  // rather than acted on from a single sample.
  test.skip(
    browserName === "webkit",
    "One green full-suite sample for this shape (3ms) against a same-run sibling failure -- below the bar to lift (SH-347, council Probe D2)",
  );
  await seedToken(page);
  const alpha = await projectSlug(page.request, "Alpha Project");
  const beta = await projectSlug(page.request, "Beta Project");
  const alphaData = await holdDataFor(page, alpha);
  // Beta's reads are held and never refused, so its own window stays open for
  // the assertions below rather than resolving out from under them.
  await holdDataFor(page, beta);
  await page.goto(
    `/?project=${encodeURIComponent(alpha)}&boardFetchTimeoutMs=${heldReadDeadlineMs()}`,
  );

  await expect(page.locator("#board-view")).toBeVisible();
  await page.locator("#drafts-btn").click();
  await alphaData.refuse();
  const list = page.locator("#drafts-list");
  await expect(list).toHaveText("No drafts.");

  // The popover's backdrop covers the topbar, and since SH-299 the keyboard
  // too, so the switch goes through the selector's own listeners — the same
  // route `board-readiness`'s own switch test takes, and for the same reason.
  await activateBehindOverlay(page.locator("#projsel-btn"));
  await expect(page.locator("#projsel-menu")).toBeVisible();
  await activateBehindOverlay(
    page.locator("#projsel-menu .projsel-item", { hasText: "Beta Project" }),
  );

  // Beta's board has not answered, but the catalog-backed Drafts surface is
  // unchanged and still makes its earned global empty claim.
  await expect(list).toHaveText("No drafts.");
  await expect(page.locator("#new-story-btn")).toHaveAttribute(
    "title",
    "Loading this project's states, types and labels…",
  );
});

/**
 * SH-292's third gap, the one its council would not let the fix leave open.
 *
 * The subject line is derived from `state.repos`, and `state.repos` is
 * refreshed by `fetchReposOnce()` — whose success path repainted the project
 * selector, the home cards and the settings table, and nothing belonging to
 * the Drafts popover. So a project renamed by another client repainted the
 * topbar and left the popover naming the old one, and *indefinitely*: the
 * board's own `/data` is a separate request, `renderAll()` runs only on a
 * parsed 200, and `markDataSettled()` fires at most once per project. A stale
 * name is worse than no name, and this is exactly the dwell state the naming
 * was added for.
 *
 * Both mocks below are data and transport, never behaviour. `/api/repos`
 * answers with the daemon's own reply, one field rewritten; `/api/events` is
 * a stream that opens and closes, so the page's real `EventSource` reconnects
 * on its own `retry:` hint and runs the page's real `es.onopen`. Every
 * repaint asserted here is the production path: `fetchReposOnce()` →
 * `state.repos` → `currentProjectLabel()` → `renderDraftsList()`.
 *
 * The dashboard has no rename endpoint of its own (`POST /api/repos`
 * registers, `DELETE` removes), so rewriting the catalog reply is what a
 * `story project set --name` on another machine looks like from here.
 */
test("a project renamed elsewhere renames its global draft rows, not just the topbar", async ({
  page,
  request,
}) => {
  await seedDraft(request, "Alpha Project", DRAFT_TITLE);
  await seedToken(page);
  // Resolved through the standalone `request` fixture, before any route is
  // registered, so the slug this test keys on cannot come from its own mock.
  const slug = await projectSlug(request, "Alpha Project");

  let name = "Alpha Project";
  await page.route("**/api/repos", async (route) => {
    // The token explicitly: `route.fetch()` replays the request without the
    // cookie the page authenticates with, and the daemon answers a 401 whose
    // body is not JSON at all.
    const response = await route.fetch({
      headers: {
        ...route.request().headers(),
        "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
      },
    });
    const repos = (await response.json()) as Array<{ id: string; name: string }>;
    repos.forEach((repo) => {
      if (repo.id === slug) repo.name = name;
    });
    await route.fulfill({ response, json: repos });
  });
  // A stream that ends immediately. `EventSource` treats EOF as a
  // disconnection and reconnects after the `retry:` it was given, so the page
  // re-runs its own `es.onopen` — `fetchReposOnce()` — every ~150ms, which is
  // how this test gets a catalog refresh without a real catalog change.
  await page.route("**/api/events", (route) =>
    route.fulfill({
      status: 200,
      headers: { "content-type": "text/event-stream", "cache-control": "no-cache" },
      body: "retry: 150\n\n",
    }),
  );

  await page.goto("/");
  await openProject(page, "Alpha Project");
  await page.locator("#drafts-btn").click();
  await expect(page.locator("#drafts-modal")).toHaveClass(/open/);
  await expect(page.locator("#drafts-list .drafts-row-project")).toHaveText(
    "AA · Alpha Project",
  );

  // From here the board's own data can repaint nothing: `renderAll()` runs
  // only on a parsed 200, and `markDataSettled()` already fired for this
  // project on the load above, so it early-returns. Without this the reconnect
  // loop's own `fetchData()` would repaint the popover for reasons that have
  // nothing to do with the catalog, and this test would pass against the
  // defect.
  await page.route(
    (url) => url.pathname === `/api/repos/${encodeURIComponent(slug)}/data`,
    (route) => route.abort(),
  );
  name = "Alpha Project Renamed Elsewhere";

  // The selector proves the refetch happened at all, so a failure on the line
  // below is the popover not repainting rather than nothing having arrived.
  await expect(page.locator("#projsel-label")).toHaveText(
    "AA · Alpha Project Renamed Elsewhere",
  );
  await expect(page.locator("#drafts-list .drafts-row-project")).toHaveText(
    "AA · Alpha Project Renamed Elsewhere",
  );
});
