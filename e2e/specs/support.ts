import { expect, test as base } from "@playwright/test";
import type {
  APIRequestContext,
  APIResponse,
  Locator,
  Page,
  Request,
  Route,
} from "@playwright/test";
import { contention, gracedTestBudget, loadGraceEnabled } from "../load-grace";

export { expect };

/** How often the load-grace watchdog (below) re-samples contention during a
 * running test. Sub-second so a burst that begins mid-test is caught with
 * room to extend the deadline before it arrives, not so tight that the
 * sampling itself is a meaningful cost against a 15s+ test budget. */
const LOAD_GRACE_SAMPLE_INTERVAL_MS = 500;

/**
 * The load-grace watchdog (SH-347's user determination, 2026-08-17: "relax
 * the timeouts when machine is under load... reset timeout timer... rather
 * than ending the test"). An auto-use fixture, applied to every test that
 * imports `test` from this file rather than from `@playwright/test`
 * directly (`tests/e2e_load_grace.rs` fences that every tracked spec does).
 *
 * Samples contention every {@link LOAD_GRACE_SAMPLE_INTERVAL_MS} and calls
 * `testInfo.setTimeout()` *before* the deadline can fire -- Playwright has no
 * hook that runs after a test's timeout has already torn it down, so "reset
 * the timer" can only mean "extend it pre-emptively." Monotonic: a grant is
 * never retracted, even if contention subsides, because a test already
 * mid-flight has no way to know it no longer needs the room. Adopts a spec's
 * own `test.setTimeout()` (e.g. `dispatch.spec.ts`'s multiple of
 * `DISPATCH_COMPLETION_TIMEOUT`) as its base the moment one is seen, so this
 * watchdog only ever ADDS grace on top of whatever budget a spec already
 * asked for, never replaces it.
 *
 * Every extension is reported on two channels -- `test.info().annotations`
 * (machine-readable, lands in the JSON/HTML reporters) and stderr (visible
 * under the `list` reporter this suite runs with) -- because a grace nobody
 * can see is the SH-306 shape one layer up: a gate whose verdict depends on
 * state it never reported.
 */
export const test = base.extend<{ loadGrace: void }>({
  loadGrace: [
    async ({}, use, testInfo) => {
      if (!loadGraceEnabled()) {
        await use();
        return;
      }
      // The un-graced floor: the budget the spec or config actually asked
      // for, before any load-driven grace is layered on top. Only updates
      // when `testInfo.timeout` changes to something this watchdog did NOT
      // just set itself -- i.e. the spec called its own `test.setTimeout()`
      // -- which is a rebase, not grace, and must never be logged as though
      // it were. Read `wantMs` fresh from THIS floor every tick, never from
      // the watchdog's own last grant: computing it from the last grant
      // would multiply an already-graced number by the ratio again on every
      // tick, compounding exponentially for as long as contention stays
      // above 1 instead of converging to one stable, ratio-appropriate
      // value.
      let floorMs = testInfo.timeout;
      let grantedMs = testInfo.timeout;
      const timer = setInterval(() => {
        if (testInfo.timeout !== grantedMs) {
          floorMs = testInfo.timeout;
          grantedMs = testInfo.timeout;
        }
        const ratio = contention();
        const wantMs = gracedTestBudget(floorMs, ratio);
        if (wantMs <= grantedMs) return;
        testInfo.setTimeout(wantMs);
        const line =
          `load-grace: extended "${testInfo.title}" to ${wantMs}ms ` +
          `(contention=${ratio.toFixed(2)}, floor=${floorMs}ms, was ${grantedMs}ms)`;
        grantedMs = wantMs;
        process.stderr.write(`${line}\n`);
        testInfo.annotations.push({ type: "load-grace", description: line });
      }, LOAD_GRACE_SAMPLE_INTERVAL_MS);
      try {
        await use();
      } finally {
        clearInterval(timer);
      }
    },
    { auto: true },
  ],
});

/**
 * Shared across every spec since SH-187: every `/api/**` route requires the
 * daemon's bearer token now, not just dispatch's own endpoint (SH-50). A
 * fresh Playwright browser context has no `sessionStorage`, so without this
 * the app's own bootstrap-time token modal would block the very first
 * `page.goto("/")` in every spec that doesn't itself test that modal.
 */

/**
 * An environment variable this suite cannot run without. Throws rather than
 * defaulting, so a spec run outside `scripts/run-e2e.sh` fails loudly
 * instead of quietly hitting a dashboard with no fixtures and no token --
 * mirrors `playwright.config.ts`'s own `DASHBOARD_URL` check.
 */
export function requiredEnv(name: string): string {
  const value = process.env[name];
  if (!value) {
    throw new Error(
      `${name} is not set — run this suite through scripts/run-e2e.sh, which starts an ` +
        "isolated daemon, seeds its fixtures, and exports the variables this file needs.",
    );
  }
  return value;
}

/**
 * Whether `scripts/run-e2e.sh` measured this machine as having WebKit's Tab
 * order include buttons and links (macOS's `AppleKeyboardUIMode >= 2`,
 * "Full Keyboard Access"), rather than its default of "text boxes and lists
 * only" -- real Safari's own out-of-box behavior for a keyboard user who has
 * never turned that setting on.
 *
 * A council decided this rather than either of the two easy wrongs (SH-335,
 * story show SH-335 carries the verdict): silently flipping the
 * developer's own System Settings would make a green/red verdict on the same
 * tree depend on unversioned machine state (the SH-306 shape -- a gate whose
 * outcome depends on something it never checked or reported); permanently
 * quarantining the affected assertions would leave the exact
 * keyboard-reachability class this suite exists to strengthen (SC 2.2.1) as
 * undated debt. So `run-e2e.sh` measures the mode once, states it loudly in
 * its own preamble, and exports the result -- this function is the one place
 * a spec reads it back, so the gate this note describes has exactly one
 * implementation to keep in sync with that preamble.
 *
 * Callers gate only the small number of assertions that need real Tab
 * traversal to reach a `<button>`/`<a>` under `webkit` specifically --
 * everything else, and every assertion under `chromium`, stays fully
 * load-bearing regardless of this machine's setting.
 */
export function fullKeyboardAccess(): boolean {
  return process.env.E2E_FULL_KEYBOARD_ACCESS === "1";
}

/**
 * A page-side read deadline (`?boardFetchTimeoutMs=`, `?catalogFetchTimeoutMs=`,
 * `?apiGetTimeoutMs=` -- `src/web_dashboard.html`) that cannot fire inside any
 * test this harness will run, for a spec that holds one of those reads across
 * its own assertions (SH-347). Derived from the running test's own timeout
 * rather than a fresh guessed number, so a held read is provably outlived by
 * the harness itself: no test can outrun `test.info().timeout`, so a page
 * clock at twice that can never be the thing that ends an assertion. Call
 * from inside a running test, after `test.info()` has a real value.
 */
export function heldReadDeadlineMs(): number {
  return test.info().timeout * 2;
}

/**
 * Seeds a named token into this page's browser context as the `HttpOnly`
 * cookie `web_dashboard.html` now rides rather than reads (SH-255) --
 * `run-e2e.sh` mints `DASHBOARD_NAMED_TOKEN` for the suite and exports the
 * cookie name the daemon actually published for it, `DASHBOARD_COOKIE_NAME`,
 * rather than this file recomputing the store-keyed digest itself.
 *
 * `context.addCookies`, not `page.addInitScript` (the pre-SH-255 shape,
 * which wrote to `sessionStorage`): an `HttpOnly` cookie cannot be set from
 * page JavaScript at all -- that is the entire point of the attribute -- so
 * this has to go in through Playwright's own browser-context API, which
 * talks to the browser directly rather than running a script in the page.
 * Set once, before the first navigation, same as the old shape required:
 * a cookie in the context's jar is sent on every subsequent request in this
 * context automatically, reload or fresh navigation alike, so unlike
 * `addInitScript` this needs no re-registration for `dispatch.spec.ts`'s
 * mid-test reload to keep seeing it.
 */
export async function seedToken(page: Page): Promise<void> {
  const token = requiredEnv("DASHBOARD_NAMED_TOKEN");
  const cookieName = requiredEnv("DASHBOARD_COOKIE_NAME");
  await page.context().addCookies([
    {
      name: cookieName,
      value: token,
      domain: "127.0.0.1",
      path: "/",
      httpOnly: true,
      sameSite: "Strict",
      secure: false,
    },
  ]);
}

/**
 * Resolves a seeded project's slug (the `id` `GET /api/repos` reports, and
 * what `?project=` names -- SH-197) from its display name. `run-e2e.sh`
 * exports the story ids and checkouts it minted, but never a project's
 * slug, and `story project new` derives one from the name by an algorithm
 * this suite has no business depending on -- so a spec that needs Alpha's
 * or Delta's actual slug asks the daemon, the same way the dashboard itself
 * would.
 */
export async function projectSlug(
  request: APIRequestContext,
  name: string,
): Promise<string> {
  const resp = await request.get("/api/repos", {
    headers: { "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN") },
  });
  const repos: Array<{ id: string; name: string }> = await resp.json();
  const match = repos.find((r) => r.name === name);
  if (!match) {
    throw new Error(`No project named "${name}" in GET /api/repos`);
  }
  return match.id;
}

/**
 * Waits until the board is showing the current project's data, rather than
 * merely showing the board.
 *
 * `selectRepo()` sets `state.data = null`, renders the screen, and *then*
 * fetches — so `#board-view` becomes visible with no stories, no metadata and
 * no vocabulary, and every spec that waited only for that was racing the
 * fetch (SH-222). Losing that race is not a slow assertion, which would
 * simply retry: the create modal is built **once**, synchronously, from
 * `meta()` at the moment it opens, so a modal opened in that window has a
 * Priority select holding nothing but "Default priority" and never
 * repopulates. `selectOption("critical")` against it then spins out the whole
 * 15s test timeout with "did not find some options" — the failure SH-223
 * recorded twice against `board-sort.spec.ts` and once against
 * `create-story-defaults.spec.ts`, each time on a machine that was busy. The
 * filter-bar dropdowns (`#fdd-states` and friends) are built from `meta()`
 * the same way, which is the same trap in `filter-persistence.spec.ts`.
 *
 * SH-265 disabled `#new-story-btn` for the whole window, closing the create-
 * modal half of this trap at the source — a real click can no longer reach
 * `openCreateModal()` before data arrives, and it refuses even a
 * programmatic one. This wait still matters for the filter-bar dropdowns,
 * which remain built from `meta()` the same way and are not gated by any
 * button; a spec that acts on them still needs to be past this line.
 *
 * The predicate is exact rather than a proxy for "some cards showed up".
 * `renderView()` writes `visible / total` into `#filter-count` only when
 * `total` is non-zero, and unhides `#empty-msg` only when there is data and
 * nothing passes the filter. So before data both are false, and after data at
 * least one is true — including for a project with no stories at all, where
 * waiting for a card would hang forever.
 */
async function waitForBoardData(page: Page): Promise<void> {
  await page.waitForFunction(() => {
    const count = document.getElementById("filter-count");
    const empty = document.getElementById("empty-msg") as HTMLElement | null;
    return Boolean(count?.textContent) || Boolean(empty && !empty.hidden);
  });
}

/**
 * Opens `name`'s board from the Home screen and waits for its data.
 *
 * The one way a spec reaches a board by clicking its card — pinned by
 * `tests/e2e_fixture_hygiene.rs`, which fails the Rust suite if a spec
 * clicks `.repo-card-name` itself, because the two lines this replaces are
 * exactly the ones that look complete and aren't (see `waitForBoardData`).
 */
export async function openProject(page: Page, name: string): Promise<void> {
  await page.locator(".repo-card-name", { hasText: name }).click();
  await expect(page.locator("#board-view")).toBeVisible();
  await waitForBoardData(page);
}

/**
 * Runs `locator`'s own click handler with no user gesture, for the specs whose
 * subject is what a navigation does to an overlay that is already open.
 *
 * Since SH-299 every overlay marks the background `inert`, so a control behind
 * an open drawer, modal or popover can be neither clicked (the backdrop takes
 * the click and dismisses the overlay instead) nor focused-and-pressed (an
 * inert element is not a focusable area). Closing that second route is the
 * entire point of that story: the dashboard used to be modal for the mouse and
 * non-modal for the keyboard, and SH-290's defect rode in on the difference.
 *
 * Several specs were built on that difference, and say so at length in their
 * own headers — they reached for the keyboard precisely because the backdrop
 * blocked the mouse. The states they arrange are still reachable without a
 * gesture: `fetchReposOnce()` calls `goHome()` on its own when the open
 * project is deleted by another client, and a deep-linked drawer can land on a
 * screen the user has already navigated to. What those tests pin is what
 * `goHome()`, `goStatuses()` and `selectRepo()` do to an open overlay however
 * they are entered, and that is worth pinning whether or not a hand can still
 * enter them.
 *
 * `dispatchEvent("click")` delivers a synthetic event straight to the element,
 * so it skips hit-testing and inertness while still running the production
 * listener — the real handler, the real function, the real render. It models no
 * user, deliberately, and each caller says so.
 *
 * Never reach for this to get past an ordinary actionability failure. A control
 * the user can actually reach must be driven the way the user reaches it, or
 * the spec stops describing the product.
 */
export async function activateBehindOverlay(locator: Locator): Promise<void> {
  await locator.dispatchEvent("click");
}

/**
 * The browser's own resolved colour for a CSS custom property, read off a
 * throwaway element rather than assumed -- the light/dark palette differs,
 * and a spec using this should keep working under whichever colour scheme
 * it actually runs in.
 *
 * Was two near-identical local copies (`story-status-light.spec.ts`,
 * `card-blockers.spec.ts`), pulled out here ahead of a third (SH-277) --
 * same shape `deleteStory`'s own comment on this file describes.
 */
export async function resolvedTokenColor(
  page: Page,
  token: string,
): Promise<string> {
  return page.evaluate((t) => {
    const probe = document.createElement("div");
    probe.style.background = `var(${t})`;
    document.body.appendChild(probe);
    const color = getComputedStyle(probe).backgroundColor;
    probe.remove();
    return color;
  }, token);
}

/**
 * Opens the filter bar's disclosure panel (SH-235) if it isn't already
 * open, and waits for it to actually render. The panel defaults collapsed
 * -- a fresh Playwright context has no localStorage, same reasoning as
 * `seedToken`'s own comment above, so every spec that drives a control
 * inside it (a priority/assignee/type/state/columns dropdown, or "Show
 * closed"/"Show archived"/"Hide empty columns") needs this first. Board
 * sort moved out of this panel entirely in SH-305 -- it's per-column now,
 * opened from each column header's own sort button, so a spec driving it
 * doesn't need this at all. `#filter-count` and `#filter-clear` are in the
 * always-visible `.filter-summary` row, not the panel -- specs that touch
 * only those don't need this at all.
 */
export async function openFilters(page: Page): Promise<void> {
  const panel = page.locator("#filter-panel");
  if (await panel.isHidden()) {
    await page.locator("#filter-toggle-btn").click();
  }
  await expect(panel).toBeVisible();
}

/**
 * Deletes the "todo"-column story titled `title` through the drawer's
 * footer Delete button and the shared delete-confirmation modal (SH-197),
 * and waits for the card to disappear. Was six near-identical copies (one
 * local `deleteStory` per spec file, plus two more inlined directly)
 * driving the drawer's now-removed inline typed-reason form, before being
 * pulled out into this one call site ahead of that form's replacement.
 */
export async function deleteStory(page: Page, title: string): Promise<void> {
  const card = page.locator('.column[data-state="todo"] .card', {
    hasText: title,
  });
  await card.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await page.locator("#drawer-footer button", { hasText: "Delete" }).click();
  await expect(page.locator("#delete-modal")).toHaveClass(/open/);
  await page.locator("#delete-reason").fill("e2e cleanup");
  await page.locator("#delete-modal-submit").click();
  await expect(page.locator("#delete-modal")).not.toHaveClass(/open/);
  await expect(card).not.toBeVisible();
}

/**
 * Deletes `title`'s "blocked"-column story through the drawer -- the same
 * shape as {@link deleteStory}, scoped to the Blocked column instead of
 * todo. SH-407: a story blocked by an `awaiting` reason or an open
 * `blocked-by`/`obviated-by` edge display-promotes out of "todo" and into
 * "blocked", which any spec that blocks its own worker story before
 * cleanup needs to account for -- three spec files each carried their own
 * copy of this before it was promoted here.
 */
export async function deleteBlockedStory(
  page: Page,
  title: string,
): Promise<void> {
  const card = page.locator('.column[data-state="blocked"] .card', {
    hasText: title,
  });
  await card.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await page.locator("#drawer-footer button", { hasText: "Delete" }).click();
  await expect(page.locator("#delete-modal")).toHaveClass(/open/);
  await page.locator("#delete-reason").fill("e2e cleanup");
  await page.locator("#delete-modal-submit").click();
  await expect(page.locator("#delete-modal")).not.toHaveClass(/open/);
  await expect(card).not.toBeVisible();
}

/**
 * Moves keyboard focus onto the open story context menu's item labelled
 * `label`, using only the keys a user has -- Home, then ArrowDown until it
 * lands.
 *
 * Only real arrow/Home/End navigation updates the menu's own roving-focus
 * bookkeeping (`storyMenuFocusIndex`), which is what ArrowRight then reads
 * to know which item's submenu to open -- so a raw `.focus()` on the
 * element would leave the menu believing focus is still elsewhere. The
 * call sites this replaces reached "Set Status" as "End, then ArrowUp
 * once", under a comment claiming that held regardless of what came after
 * it -- true only while Delete was the menu's last item, and false the
 * moment a second submenu (SH-310) was inserted between them. A spec
 * should name the item it means, not count from an end that can move.
 *
 * Scoped to the main menu, not `.ctxmenu` bare: `.ctxmenu-sub` carries that
 * same class, so an unscoped locator is a strict-mode violation whenever a
 * submenu is already open.
 */
export async function focusMenuItemByLabel(
  page: Page,
  label: string,
): Promise<Locator> {
  const menu = page.locator(".ctxmenu:not(.ctxmenu-sub)");
  await expect(menu).toHaveCount(1);
  const items = menu.locator(".ctxmenu-item");
  const target = items.filter({ hasText: label });
  await expect(target).toHaveCount(1);

  const count = await items.count();
  await page.keyboard.press("Home");
  for (let i = 0; i < count; i += 1) {
    if (await target.evaluate((node) => node === document.activeElement)) {
      return target;
    }
    await page.keyboard.press("ArrowDown");
  }
  throw new Error(
    `focusMenuItemByLabel: "${label}" never took focus after ${count} ` +
      "ArrowDown presses -- the menu's roving focus is not moving, or the " +
      "label matches a separator",
  );
}

/**
 * How far ahead of the page's own clock {@link freezeClock} aims.
 *
 * A lead is not a choice, it is forced by the API: `clock.pauseAt` is a
 * jump-then-pause primitive whose jump must be forward, and the clock keeps
 * ticking while `Date.now()` round-trips to this process — so
 * `pauseAt(pageNow)` fails with `Cannot fast-forward to the past` every time.
 * Measured, not reasoned about: it failed on all four probes of SH-318's spike
 * before the lead was added.
 *
 * The value satisfies an inequality rather than modelling anything, which is
 * why it is a bound and not a fudge factor. It must exceed one test-to-page
 * round trip (single-digit to tens of milliseconds, even loaded) and stay well
 * under the shortest interval the jump would otherwise fire — the dashboard's
 * 15s SSE watchdog, and far below its 50s staleness threshold. Two orders of
 * magnitude of headroom at each end. The only timer this jump does fire is the
 * 1s footer tick, once.
 *
 * Nothing asserts on it. A caller freezes *before* the behaviour under test
 * exists, so the lead is spent on an empty page and no margin anywhere depends
 * on its value.
 */
const FREEZE_LEAD_MS = 2000;

/**
 * Pauses the page's clock, so that from here on time advances only when a test
 * says so with `page.clock.runFor()`.
 *
 * Requires `page.clock.install()` to have run before the page was navigated.
 * Prefer {@link onAFrozenClock}, which cannot leave the clock paused.
 */
async function freezeClock(page: Page): Promise<void> {
  await page.clock.pauseAt((await page.evaluate(() => Date.now())) + FREEZE_LEAD_MS);
}

/**
 * Runs `body` with the page's clock frozen, and resumes it afterwards whatever
 * happens.
 *
 * This is the suite's timer-side equivalent of {@link latch} and
 * {@link holdFetch}: a boundary the *test* decides, in place of a wall-clock
 * wait it can only hope to out-race. `latch`'s own comment gives the reasoning
 * those helpers were built on, and it applies identically to timers — a spec
 * that sleeps `SUCCESS_VISIBLE_MS + FADE_MS + 1500` is out-racing the machine,
 * and loses that race on a machine running three or four worktree sessions
 * (SH-318). Inside a frozen window the arithmetic is not merely robust to load,
 * it is independent of it: fake time advances only on `runFor`, so there is no
 * drift to size a margin against and no tail for a loaded machine to hide in.
 *
 * **The `finally` is load-bearing, not tidiness.** `install()` is per-
 * BrowserContext and covers the whole page, so a frozen clock left frozen
 * stalls every timer the teardown needs — the drawer close, the board
 * re-render, the delete flow. A failing assertion inside an unscoped frozen
 * window would therefore turn one honest red into a hang plus a stranded
 * fixture story in a shared project: a false second symptom on top of the true
 * one, and `cleanUpCreatedStories` exists because that is expensive here
 * (SH-245).
 */
export async function onAFrozenClock(
  page: Page,
  body: () => Promise<void>,
): Promise<void> {
  await freezeClock(page);
  try {
    await body();
  } finally {
    await page.clock.resume();
  }
}

/**
 * The granularity of a timestamp the store itself wrote, in milliseconds,
 * read off the value rather than assumed.
 *
 * `service::Clock::System` formats RFC3339 at **second** precision -- "the
 * format every storyhook timestamp has always used" (`src/service/mod.rs`) --
 * so today every sample lands in the no-fraction branch and this answers
 * 1000. It is derived anyway because the number is shared with production
 * code that is free to change it, and a hand-copied constant on the far side
 * of a language boundary is precisely the drift SH-136 and SH-312 each cost
 * this project (a dashboard deadline and a daemon ceiling, hand-copied, then
 * silently disagreeing). A store that moved to milliseconds would report
 * `.123Z` here and shorten {@link waitUntilStoreClockPasses}'s wait by three
 * orders of magnitude with no edit.
 *
 * The derivation only reads *finer* than a second: a sample carries its own
 * fractional digits, but nothing in a whole-second string distinguishes a
 * one-second clock from a one-minute one. A clock coarser than a second
 * would need this read from the daemon instead of inferred -- noted rather
 * than built, since no such clock exists or is proposed.
 */
function timestampResolutionMs(at: string): number {
  const fraction = /\.(\d+)/.exec(at);
  if (!fraction) return 1000;
  return Math.max(1, 10 ** (3 - fraction[1].length));
}

/**
 * Waits until the wall clock has left the instant `at` names, so that any
 * store write issued after this resolves is stamped **strictly later** than
 * `at` -- not merely "probably later".
 *
 * This is the ordering-side companion to {@link latch} and
 * {@link onAFrozenClock}, and it exists because a one-second clock makes
 * "touched after" unrepresentable inside its own second (SH-329). Originally
 * that mattered because the pre-SH-336 "Modified" sort broke a same-second
 * tie on story number ascending, so two writes in one second sorted by
 * *creation* order -- exactly what the sort under test was supposed to
 * disprove. `board-sort.spec.ts` created two stories and touched one in
 * 1.9s of wall clock, all three writes stamped `13:31:33Z`, and the board
 * dutifully returned creation order: red, on a correct board, roughly half
 * the time. Warmer machines failed it *more* often, having fitted all three
 * writes into one second more easily -- which is why three sessions read it
 * as load-related and none of them reproduced it that way.
 *
 * SH-336 gave the board an exact same-second tiebreak (`head_global_seq`),
 * which resolved that flake -- but this helper stayed load-bearing for a
 * different reason: the tiebreak fires off *which sort is selected*
 * ("Modified"), not off which timestamp field actually produced a tie. A
 * spec that wants to prove the sort reads `updated_at` rather than
 * `created_at` still needs the two to diverge, or the tiebreak alone would
 * mask a wrong field read. `board-sort-tiebreak.spec.ts` covers the
 * tiebreak's own correctness, forcing the exact tie this helper exists to
 * dodge everywhere else.
 *
 * Note what this is not. It is not a margin sized against the machine, the
 * shape SH-245 and SH-318 removed from this suite: those sleeps are bets
 * that an action finishes inside a guessed budget, and a loaded machine
 * wins them. This waits for a boundary that arrives on its own schedule
 * whatever the load, and waiting *longer* than needed is always harmless --
 * it only ever makes the following write later. The cost is bounded by one
 * resolution tick, under a second today.
 *
 * The wait runs on Node's clock, not the page's, so a spec that has
 * installed Playwright's clock emulation cannot accidentally freeze it --
 * and Node, the daemon and the store all read the same host clock, so
 * there is no skew to budget for.
 */
export async function waitUntilStoreClockPasses(at: string): Promise<void> {
  const instant = Date.parse(at);
  if (Number.isNaN(instant)) {
    throw new Error(
      `waitUntilStoreClockPasses: "${at}" is not a timestamp Date.parse understands`,
    );
  }
  const remaining = instant + timestampResolutionMs(at) - Date.now();
  if (remaining > 0) {
    await new Promise((resolve) => setTimeout(resolve, remaining));
  }
}

/**
 * A one-shot latch: `held` stays pending until `release()` is called, and
 * every subsequent `await` on it resolves immediately.
 *
 * The suite uses this to open a race window a test needs — a request held
 * in flight while the test types into the drawer, say — with a boundary the
 * *test* decides rather than a `setTimeout`. A wall-clock delay only works
 * while the test out-races it: SH-245's specs each budgeted ~500ms for a
 * Playwright action that normally takes 8ms, which is ample until the
 * machine is loaded, and which fails in two directions at once. Lose the
 * race narrowly and the assertion goes red for a reason that has nothing to
 * do with the behaviour under test; lose it widely and the window has
 * already closed before the test acts, so the spec passes while exercising
 * nothing — the worse of the two, because it is silent.
 */
export function latch(): { held: Promise<void>; release: () => void } {
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  return { held, release };
}

/**
 * Holds every `GET .../story/<id>` drawer-detail fetch in flight until the
 * returned function is called, then lets it through to the daemon.
 *
 * The drawer renders once synchronously from cached summary data and again
 * when this fetch resolves (SH-218); holding it is how a spec puts the
 * second render exactly where it wants it. Register before the click that
 * opens the drawer, release once the drawer is in the state under test.
 */
export async function holdDetailFetch(page: Page): Promise<() => void> {
  const gate = latch();
  await page.route(/\/story\/[^/]+$/, async (route) => {
    if (route.request().method() !== "GET") {
      await route.continue();
      return;
    }
    await gate.held;
    await route.continue();
  });
  return gate.release;
}

/** A real reply from the daemon, taken when the page asked for it and
 * delivered when the test says so. See {@link holdFetch}. */
export interface HeldFetch {
  /** Resolves once the daemon has answered the held request -- i.e. once
   * the snapshot the page will eventually receive has been taken. */
  taken: Promise<void>;
  /**
   * Refuses every *later* matching fetch, and resolves once the ones already
   * in flight have landed.
   *
   * Without this a staleness spec proves nothing: a mutation is followed by
   * its own fetch and, `FETCH_DEBOUNCE_MS` later, the SSE-driven one, so a
   * view this test un-renders would simply be re-rendered by the next reply
   * to arrive, and a retrying assertion would never see the gap. Refusing
   * them models the production case exactly -- there, the fetches that could
   * repair the view have *already been spent* on the write, and the next one
   * is a safety poll 25 seconds out, far beyond any assertion budget.
   */
  seal: () => Promise<void>;
  /** Delivers the held reply to the page, and resolves once the page's own
   * `onreadystatechange` has had its turn on the renderer's task queue. */
  deliver: () => Promise<void>;
}

/**
 * Holds the first GET at a URL `matches` accepts whose body satisfies
 * `until`, and answers every matching request before it from the daemon as
 * usual.
 *
 * The dashboard applies replies in the order they *arrive* and has several in
 * flight at once, so on a loaded machine an older one can land last. This puts
 * that ordering where the test decides rather than where the machine's load
 * happens to put it. Nothing is faked: the real reply is taken from the real
 * daemon at the moment the page asked for it, and those same bytes are
 * delivered later.
 *
 * The predicate is what makes such a spec deterministic. A mutation produces
 * *two* fetches (its own and the SSE-driven one), and the previous mutation's
 * second fetch can still be to come when this arms -- so "the next fetch" is
 * not a fixed snapshot, and a spec that assumed it was would sometimes hold
 * one taken before the write it is about had even happened. The test says
 * which reply it needs instead, and the harness waits for it.
 *
 * `route.fetch()` rather than a delayed `route.continue()`: the point is a
 * reply *taken* before a later write and *delivered* after it, and
 * `continue()` would send the request only once released, by which time the
 * daemon would answer with the write already in it -- a test that exercises
 * nothing (`latch()`'s own doc comment names the same failure shape).
 *
 * GETs only. A URL the page reads is not always one it only reads --
 * `/api/repos` is both the project list and the endpoint that creates a
 * project -- and holding a write would stall the very mutation a spec is
 * arranging rather than the reply it means to delay.
 */
export async function holdFetch<T>(
  page: Page,
  matches: (url: URL) => boolean,
  until: (body: T) => boolean,
  options?: { sealOnHold?: boolean },
): Promise<HeldFetch> {
  const taken = latch();
  const gate = latch();
  const isHeldFetch = (request: Request) =>
    request.method() === "GET" && matches(new URL(request.url()));
  let heldRequest: Request | null = null;
  let sealed = false;

  // Matching fetches still in flight. `seal()` drops the held one (which has
  // no reply until `deliver()`) and waits for the rest to empty, because a
  // reply already on the wire when the seal goes up would repair the view
  // just as effectively as one issued after it.
  const outstanding = new Set<Request>();
  page.on("request", (request) => {
    if (isHeldFetch(request)) outstanding.add(request);
  });
  const settled = (request: Request) => outstanding.delete(request);
  page.on("requestfinished", settled);
  page.on("requestfailed", settled);

  /** What one candidate turned out to be: not the one (`pass`), an earlier
   * reply to answer immediately (`answer`), or the one to hold (`hold`). */
  type Decision =
    | { kind: "pass" }
    | { kind: "answer" | "hold"; response: APIResponse };

  /**
   * Decides a single candidate, and claims the hold if it is the one.
   *
   * Kept apart from the handler because this half must run for one request at
   * a time (see `decisions` below) while the *waiting* half must not.
   */
  const decide = async (route: Route): Promise<Decision> => {
    if (heldRequest) return { kind: "pass" };
    // The bearer token by header, because the page's own credential is the
    // `HttpOnly` cookie (SH-255) and `route.fetch()` does not carry the
    // browser context's jar -- it answers 401, whose body the page then
    // fails to parse, so the reply is never applied and the test passes
    // having proved nothing. That vacuous pass is what this closes; the
    // daemon accepts either channel (`src/api/admission.rs`), and the bytes
    // are still the daemon's own.
    const response = await route.fetch({
      headers: {
        ...route.request().headers(),
        "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
      },
    });
    if (!response.ok()) {
      throw new Error(
        `holdFetch: the daemon answered ${response.status()} for a reply ` +
          "this spec depends on — it would prove nothing",
      );
    }
    if (!until((await response.json()) as T))
      return { kind: "answer", response };
    heldRequest = route.request();
    // Sealing here rather than from the test body closes a window the test
    // body cannot: a reply that lands between the hold and a later `seal()`
    // is applied, and the held one is then older than it -- so the
    // *ordering* half of a guard would answer, and a spec meant to exercise
    // the other half would pass without ever reaching it.
    if (options && options.sealOnHold) sealed = true;
    taken.release();
    return { kind: "hold", response };
  };

  /**
   * Serializes `decide`, so exactly one request is ever held.
   *
   * `until` needs the reply's body, which needs an await, and two candidates
   * inside that window both pass the check and both become "the held one":
   * the second overwrites `heldRequest`, and the first then waits on `gate`
   * forever, because `deliver()` releases one request and only one. Never
   * fulfilled and never failed, it never leaves `outstanding` either, so
   * `seal()` waits out its whole budget on a request nothing will ever
   * settle -- a red spec whose message names this harness and not the
   * behaviour under test.
   *
   * Two replies satisfying one predicate at the same moment is the ordinary
   * case rather than a rare one: a mutation is followed by its own fetch and,
   * `FETCH_DEBOUNCE_MS` later, the SSE-driven one, and both carry the write
   * the predicate is looking for. It stays invisible until the machine is
   * loaded enough for the first `route.fetch()` to still be in flight when
   * the second request arrives, which is the same condition the specs using
   * this exist to reproduce.
   */
  let decisions: Promise<unknown> = Promise.resolve();

  await page.route(matches, async (route) => {
    if (!isHeldFetch(route.request())) {
      await route.continue();
      return;
    }
    if (heldRequest) {
      // Refused, not held, once sealed: an aborted fetch leaves the applied
      // state untouched (the page's own `onerror` sets the connection flag
      // and nothing else), which is precisely a view with no repair on the
      // way. Answered before the queue, so a slow decision in front of it
      // cannot keep a request whose outcome is already known in flight.
      if (sealed) await route.abort();
      else await route.continue();
      return;
    }
    const decided = decisions.then(() => decide(route));
    // The chain must survive a rejected decision, or every candidate behind
    // it inherits that failure; this handler still sees its own.
    decisions = decided.catch(() => undefined);
    const outcome = await decided;
    if (outcome.kind === "pass") {
      if (sealed) await route.abort();
      else await route.continue();
      return;
    }
    // Held outside the queue: a request waiting for `deliver()` would
    // otherwise block every candidate behind it from being answered at all,
    // and `seal()` would wait for exactly the requests it just sealed off.
    if (outcome.kind === "hold") await gate.held;
    await route.fulfill({ response: outcome.response });
  });

  return {
    taken: taken.held,
    seal: async () => {
      sealed = true;
      if (heldRequest) outstanding.delete(heldRequest);
      await expect.poll(() => outstanding.size).toBe(0);
    },
    deliver: async () => {
      // Identity, not the URL: every fetch of this kind in this page shares
      // one URL, so a URL match could be satisfied by an unrelated reply that
      // happened to be in flight, and the assertion below would then read the
      // DOM before the held body ever reached it.
      const arrived = page.waitForResponse((r) => r.request() === heldRequest);
      gate.release();
      await arrived;
      // The XHR's completion task was queued on the renderer before this
      // `evaluate` could be, and a renderer runs its task queue in order, so
      // a macrotask that resolves here is proof the page has already done
      // whatever it intends to do with that body.
      await page.evaluate(() => new Promise((r) => setTimeout(r, 0)));
    },
  };
}

/** A request held in flight until the test decides its fate. See
 * {@link holdUntilRefused}. */
export interface HeldRoute {
  /** Refuses the held request, and every later one matching the same
   * predicate, so nothing repairs the view behind the assertions that
   * follow. */
  refuse: () => void;
  /** Stops refusing: from here on these requests reach the daemon, which is
   * how a spec puts a store back in service under a page that has already
   * given up on it. */
  restore: () => void;
}

/**
 * Holds every request matching `matches` in flight until `refuse()` is
 * called, then aborts it and every later match -- until `restore()` says
 * otherwise.
 *
 * A simpler tool than {@link holdFetch} above: this one answers no requests
 * from the daemon at all, it only ever holds-then-aborts or holds-then-lets-
 * through, which is what a readiness spec needs to drive a fetch through
 * "never completed" and back to "answers again" without caring about the
 * ordering of several in-flight replies. Originated as `board-readiness.spec.ts`'s
 * own `holdDataFor` (SH-291, scoped to one project's `/data`); generalised
 * here to whatever URL a spec needs, since SH-301's catalog and
 * statuses-editor specs both need the identical lifecycle on different
 * routes.
 *
 * `restore()` flips a flag the handler reads rather than removing the route:
 * `page.unroute()` identifies a route by the matcher it was registered with,
 * and a URL *predicate* is a function, so a second one spelled identically is
 * a different route and removes nothing -- a spec that "restored" that way
 * would sit and watch its requests go on being aborted until the assertion
 * timed out, blaming the behaviour under test rather than the harness.
 */
export async function holdUntilRefused(
  page: Page,
  matches: (url: URL) => boolean,
): Promise<HeldRoute> {
  const gate = latch();
  let refusing = true;
  await page.route(matches, async (route) => {
    await gate.held;
    if (refusing) await route.abort();
    else await route.continue();
  });
  return {
    refuse: gate.release,
    restore: () => {
      refusing = false;
    },
  };
}

/**
 * Board stories, as `GET .../data` reports them: open and closed alike,
 * neither deleted nor draft (`project_data_json` in `src/api/rest.rs`).
 */
interface BoardStory {
  id: string;
  superstate: string;
}

/** Reads `projectName`'s board stories and drafts, by id. */
async function storiesInProject(
  request: APIRequestContext,
  projectName: string,
): Promise<BoardStory[]> {
  const slug = await projectSlug(request, projectName);
  const resp = await request.get(
    `/api/repos/${encodeURIComponent(slug)}/data`,
    { headers: { "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN") } },
  );
  if (!resp.ok()) {
    throw new Error(
      `GET /data for "${projectName}" answered ${resp.status()}: ${await resp.text()}`,
    );
  }
  const data = await resp.json();
  const views = [...(data.stories ?? []), ...(data.drafts ?? [])];
  return views.map((view: { story: BoardStory }) => ({
    id: view.story.id,
    superstate: view.story.superstate,
  }));
}

/**
 * The stories each project held the first time a spec asked — the seeded
 * fixture, since every spec that creates one registers the cleanup below
 * and so cannot leave one behind for the next spec's baseline to absorb.
 *
 * Module state, so it is captured once for the whole run rather than once
 * per file. That relies on `workers: 1` (`playwright.config.ts`): a second
 * worker would take its own baseline in its own process, at whatever moment
 * its first test ran, which is not necessarily a pristine board.
 */
const fixtureBaselines = new Map<string, Set<string>>();

/**
 * Registers an `afterEach` that deletes, through the API, every story the
 * spec left behind in `projectName` — anything not in the fixture the run
 * started with. Call once at the top of any spec file that creates stories
 * in a project other specs also read.
 *
 * A test body's own `deleteStory()` call is the last statement it runs, so
 * any failure above it strands the story it created. SH-245 is what that
 * costs: one red spec became three, because the strays inflated Alpha's
 * two-story fixture and `filter-persistence.spec.ts` asserts on the count
 * (`0 / 2` read `0 / 3`) — a failure naming a project switch that was never
 * involved, in a file the actual defect never touched. An `afterEach` runs
 * whether the test passed or failed, so a stray cannot outlive the test
 * that created it, and a red spec stays one red spec.
 *
 * A CLOSED stray is reopened first, then deleted (SH-222). Both halves of
 * the rule this sweep originally skipped them under were wrong: closing a
 * story sets the store row's `archived` flag (`StoryClosedAndArchived`), and
 * `StoryService::delete` answers *404, not a refusal*, for such a row — so
 * a bare `DELETE` here would have failed loudly for a story that is very
 * much still there. And it is not "counted by nothing": `/data` excludes
 * only deleted and draft stories, so a closed stray sits in
 * `state.data.stories` and inflates `#filter-count`'s denominator for the
 * rest of the run. That is the difference between SH-245's symptom and
 * SH-223's: an open stray was swept by the next test, a closed one never
 * was, so every later count assertion in the run read one too many.
 */
export function cleanUpCreatedStories(projectName: string): void {
  test.beforeEach(async ({ request }) => {
    if (fixtureBaselines.has(projectName)) return;
    const baseline = await storiesInProject(request, projectName);
    fixtureBaselines.set(projectName, new Set(baseline.map((s) => s.id)));
  });

  test.afterEach(async ({ request }) => {
    const baseline = fixtureBaselines.get(projectName);
    if (!baseline) return;
    const slug = await projectSlug(request, projectName);
    // `X-Storyhook` as well as the token: a mutation also has to clear
    // `mutation_guard_ok`'s CSRF check, which a read does not
    // (`src/api/admission.rs`). Without it these answer 403.
    const headers = {
      "X-Storyhook": "1",
      "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
    };
    const storyUrl = (id: string) =>
      `/api/repos/${encodeURIComponent(slug)}/story/${encodeURIComponent(id)}`;

    for (const story of await storiesInProject(request, projectName)) {
      if (baseline.has(story.id)) continue;
      if (story.superstate !== "OPEN") {
        const reopened = await request.post(`${storyUrl(story.id)}/reopen`, {
          headers,
          data: {},
        });
        if (!reopened.ok()) {
          throw new Error(
            `cleanUpCreatedStories: POST ${story.id}/reopen answered ` +
              `${reopened.status()}: ${await reopened.text()}`,
          );
        }
      }
      const deleted = await request.delete(storyUrl(story.id), {
        headers,
        data: { reason: "e2e afterEach cleanup (SH-245)" },
      });
      // Loud on failure: a cleanup that quietly gives up leaves exactly the
      // stray it exists to remove, and the next spec pays for it instead.
      if (!deleted.ok()) {
        throw new Error(
          `cleanUpCreatedStories: DELETE ${story.id} answered ` +
            `${deleted.status()}: ${await deleted.text()}`,
        );
      }
    }
  });
}

/**
 * Fixtures for the two specs whose subject is what a key press does to a
 * dashboard form -- `modal-enter-autorepeat.spec.ts` (SH-362, the auto-repeat
 * bit) and `ime-composition-keys.spec.ts` (SH-368, the composition bit).
 *
 * Lifted here when the second file needed them, rather than duplicated, for the
 * reason the notice helpers above were: two copies of a fixture are two things
 * that can disagree about what a surface looks like, and the surfaces they open
 * (the delete modal, the statuses screen, the token modal) are shared product
 * surfaces rather than either story's own.
 */

/** The headers a direct API call needs: the suite's bearer token, plus the
 * `X-Storyhook` header a *mutation* must also carry to clear
 * `mutation_guard_ok`'s CSRF check (`src/api/admission.rs`). */
function apiHeaders(): Record<string, string> {
  return {
    "X-Storyhook": "1",
    "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
  };
}

/** Opens the shared delete modal on `card` through the drawer footer, and types
 * `reason` (pass `""` to leave the field for the caller to drive itself). */
export async function openDeleteModal(
  page: Page,
  card: Locator,
  reason: string,
): Promise<void> {
  await card.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await page.locator("#drawer-footer button", { hasText: "Delete" }).click();
  await expect(page.locator("#delete-modal")).toHaveClass(/open/);
  if (reason) await page.locator("#delete-reason").fill(reason);
}

/** Opens Settings and drills into `project`'s statuses screen. */
export async function openStatuses(page: Page, project: string): Promise<void> {
  await page.locator("#settings-btn").click();
  await expect(page.locator("#settings-view")).toBeVisible();
  await page
    .locator(".settings-table tbody tr", { hasText: project })
    .getByRole("button", { name: "Statuses" })
    .click();
  await expect(page.locator(".settings-head h2")).toHaveText(`Statuses · ${project}`);
}

/** Removes a scratch status through the API -- it holds no stories, so nothing
 * moves. Loud on failure, for `cleanUpCreatedStories`'s own reason: a cleanup
 * that gives up quietly leaves the stray it exists to remove, and the next spec
 * pays for it. */
export async function deleteStatus(
  request: APIRequestContext,
  project: string,
  slug: string,
): Promise<void> {
  const projectId = await projectSlug(request, project);
  const resp = await request.delete(
    `/api/repos/${encodeURIComponent(projectId)}/states/${encodeURIComponent(slug)}`,
    { headers: apiHeaders(), data: {} },
  );
  if (!resp.ok()) {
    throw new Error(
      `deleteStatus: DELETE ${slug} answered ${resp.status()}: ${await resp.text()}`,
    );
  }
}

/** Answers the page's first `GET /api/repos` with a 401, so `api()`'s own 401
 * handler opens the token modal.
 *
 * A stubbed *reply*, never stubbed behaviour: the modal that opens, the exchange
 * it performs and the retry it schedules are all the page's real code. The
 * alternative -- running with no seeded credential -- is deliberately left to
 * `loopback-requires-a-token.spec.ts`, which is the one spec in this suite that
 * authenticates nothing and says so in its own header. */
export async function refuseTheFirstReposRead(page: Page): Promise<void> {
  let refused = false;
  await page.route(/\/api\/repos$/, async (route) => {
    if (refused || route.request().method() !== "GET") {
      await route.continue();
      return;
    }
    refused = true;
    await route.fulfill({
      status: 401,
      contentType: "application/json",
      body: JSON.stringify({ error: "token required" }),
    });
  });
}

/**
 * Notice-raising helpers, shared between `notice-dock-geometry.spec.ts` (SH-323)
 * and `notice-announcement.spec.ts` (SH-333). Lifted here rather than duplicated
 * when the second file needed them.
 */

/** Turns durable notices on through the real Settings control.
 *
 * Through the page rather than by seeding `localStorage`: SH-322 shipped this as
 * a mechanism the user can reach, and a seeded key would satisfy the storage
 * half of that claim while proving no mechanism exists at all. */
export async function keepNotices(page: Page): Promise<void> {
  await page.locator("#settings-btn").click();
  await expect(page.locator("#settings-view")).toBeVisible();
  await page.locator("#toggle-keep-notices").click();
  await expect(page.locator("#toggle-keep-notices")).toBeChecked();
  await page.locator("#home-btn").click();
}

/** Raises one durable notice per call through the story context menu, using a
 * different copy target each time so the notices are distinguishable by text.
 *
 * The three labels are the whole reason this path is used rather than three
 * Copy-IDs: `copyText` names the target in its headline, so "ID copied", "URL
 * copied" and "Description copied" are three notices an order assertion can tell
 * apart. Three identical "ID copied" notices could not pin an order at all. */
export const COPY_TARGETS = ["Copy ID", "Copy URL", "Copy Description"] as const;
export const COPY_HEADLINES = ["ID copied", "URL copied", "Description copied"] as const;

export async function raiseNotice(page: Page, title: string, which: number): Promise<void> {
  const card = page.locator('.column[data-state="todo"] .card', { hasText: title });
  await card.click({ button: "right" });
  await page.locator(".ctxmenu-item", { hasText: COPY_TARGETS[which] }).click();
}

/** Holds `key` down: one deliberate press, then `repeats` auto-repeats, then
 * release.
 *
 * Playwright reports the first `down` with `repeat: false` and every subsequent
 * one with `repeat: true`, which is the sequence a held key produces and the
 * only part of it this harness can speak to. What a test built on this proves
 * is what the page does with a repeat-flagged event; that a *physically* held
 * key sets the flag rests on the UI Events spec and on a hand check, and is not
 * evidence this suite can produce (the SH-322/SH-327 precedent).
 *
 * Lived in `notice-autorepeat.spec.ts` (SH-339) until `modal-enter-autorepeat
 * .spec.ts` (SH-362) needed the identical primitive — the same journey
 * `deleteStory` and `resolvedTokenColor` above already made. */
export async function holdKey(page: Page, key: string, repeats: number): Promise<void> {
  await page.keyboard.down(key);
  for (let i = 0; i < repeats; i++) await page.keyboard.down(key);
  await page.keyboard.up(key);
}

/** Waits until no backdrop is still on screen.
 *
 * Closing an overlay is a 0.18s opacity transition, and `.backdrop` carries no
 * `pointer-events: none` — so for those 180ms a full-viewport `position: fixed;
 * inset: 0` element is still the thing under the cursor everywhere. A hit test
 * taken in that window reports the backdrop and says nothing about the dock.
 * Found by `notice-dock-geometry.spec.ts`'s own failure message naming
 * `div#modal-backdrop.backdrop`. */
export async function awaitNoOverlay(page: Page): Promise<void> {
  await page.waitForFunction(
    () => document.querySelectorAll(".backdrop:not([hidden])").length === 0,
    undefined,
    { timeout: 5000 },
  );
}

/** Creates a story with both a title and a description (so Copy Description has
 * something to copy) and returns its id.
 *
 * Sets a priority (SH-358): this helper is a general-purpose fixture used by
 * 25+ unrelated specs across this suite, none of which test priority. Left at
 * "Default priority" it silently raises the unassessed-priority warning toast
 * on every call -- a side effect no caller here is testing for, and one that
 * broke a dozen specs whose own notice-count and focus-geometry assertions
 * didn't expect an extra, timer-driven toast in the stack. A story actually
 * testing priority behaviour (story-context-menu-priority.spec.ts) has its
 * own local `createStory` with an optional priority argument and is
 * unaffected by this one. */
export async function createStory(page: Page, title: string): Promise<string> {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  await page.locator("#create-description").fill("a description, so Copy Description has something to copy");
  await page.locator("#create-priority").selectOption("medium");
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  const card = page.locator('.column[data-state="todo"] .card', { hasText: title });
  await expect(card).toBeVisible();
  await awaitNoOverlay(page);
  return (await card.getAttribute("data-id"))!;
}

/** Raises `count` further durable notices, asserting the running total after
 * each one.
 *
 * Counted from whatever is already standing rather than from zero: a helper that
 * assumed an empty stack would silently pass while measuring the wrong pile the
 * first time a caller topped one up. Asserting after every raise is what keeps a
 * geometry assertion from racing a stack that is still growing under it. */
export async function raiseDurableNotices(page: Page, title: string, count: number): Promise<void> {
  const start = await page.locator("#toast-stack .toast").count();
  for (let i = 0; i < count; i++) {
    await raiseNotice(page, title, i % COPY_TARGETS.length);
    await expect(page.locator("#toast-stack .toast")).toHaveCount(start + i + 1);
  }
}

/**
 * Focus-contrast instrument (SH-338), lifted here ahead of a second caller
 * (SH-360) -- the same journey `deleteStory` and `resolvedTokenColor` above
 * already made ("was N near-identical copies... pulled out here").
 *
 * `getComputedStyle` is read for the resolved `outline-color`, and the
 * backdrop is **composited from the real ancestor chain** rather than
 * assumed to be any one token: a button's own background is often
 * `transparent`, so what is actually behind its ring is whatever the
 * ancestor chain happens to paint, which is the thing that can change
 * without anyone editing the focus rule itself.
 */

/** A colour with a straight (non-premultiplied) alpha, in sRGB 0-255. */
export type Rgba = { r: number; g: number; b: number; a: number };

/** Everything the browser will say about one element's focus indicator. */
export type IndicatorProbe = {
  focusVisible: boolean;
  outlineStyle: string;
  outlineWidth: string;
  outlineColor: string;
  outlineOffset: string;
  /** `border-top-color`, taken as representative of the whole border: every
   * control this probe measures declares a uniform `border:` shorthand, so
   * the four sides never differ. */
  borderColor: string;
  /** `background-color` from the element outward to `<html>`, in that order. */
  backgrounds: string[];
  /** For failure messages: what the probe actually landed on. */
  what: string;
};

/** Reads the rendered indicator off whatever `selector` resolves to.
 *
 * `":focus"` is a legal argument and is how a caller can name "whatever holds
 * focus" without restating an implementation's own identity -- SH-338's
 * inheritance tests use it because the heir is chosen by the page at
 * dismissal time, and naming it by class would ask the page a question it
 * has already answered differently. */
export async function probeIndicator(page: Page, selector: string): Promise<IndicatorProbe | null> {
  return page.evaluate((sel) => {
    const el = document.querySelector(sel) as HTMLElement | null;
    if (!el) return null;

    // Waits for any CSS transition touching a colour this probe reads to
    // settle before measuring (SH-360). `.search-input`/`.drawer-title`/
    // `.description-field` transition `border-color`/`background` over
    // 0.15s, and `.column` (a `.card` ring's PARENT backdrop since the
    // outline-offset fix above) transitions `background` the same way.
    // Switching this page's theme WHILE a control holds focus re-triggers
    // that transition -- the custom property a transitioning declaration
    // resolves to changes value mid-animation -- so reading computed style
    // immediately after `THEMES[n].apply()` can catch an interpolated,
    // half-transparent colour rather than the theme's real one (caught by a
    // probe run that printed alphas like 0.035 on a border that should have
    // been fully opaque). Polled via `requestAnimationFrame` rather than a
    // fixed sleep -- this suite's own doctrine against a magic-number wait
    // (`notice-dock-geometry.spec.ts`'s scroll-position polling makes the
    // identical argument) -- with a ceiling generous enough to clear every
    // transition duration in this sheet (max observed: 0.2s) many times
    // over, so a signature that never actually settles still resolves with
    // its last sample rather than hanging the test forever.
    const sample = () => {
      const cs = getComputedStyle(el);
      const backgrounds: string[] = [];
      for (let n: Element | null = el; n; n = n.parentElement) {
        backgrounds.push(getComputedStyle(n).backgroundColor);
      }
      return { cs, backgrounds };
    };
    const signatureOf = (cs: CSSStyleDeclaration, backgrounds: string[]) =>
      [cs.outlineStyle, cs.outlineWidth, cs.outlineColor, cs.outlineOffset, cs.borderTopColor, backgrounds.join(",")].join("|");

    const MAX_ATTEMPTS = 120; // ~2s at 60fps -- 10x the sheet's slowest colour transition
    const STABLE_FRAMES_NEEDED = 3;

    return new Promise<IndicatorProbe>((resolve) => {
      let lastSignature = "";
      let stableFrames = 0;
      let attempts = 0;
      const tick = () => {
        attempts++;
        const { cs, backgrounds } = sample();
        const signature = signatureOf(cs, backgrounds);
        if (signature === lastSignature) {
          stableFrames++;
        } else {
          stableFrames = 0;
          lastSignature = signature;
        }
        if (stableFrames >= STABLE_FRAMES_NEEDED || attempts >= MAX_ATTEMPTS) {
          const cls = typeof el.className === "string" && el.className ? `.${el.className.split(" ").join(".")}` : "";
          resolve({
            focusVisible: el.matches(":focus-visible"),
            outlineStyle: cs.outlineStyle,
            outlineWidth: cs.outlineWidth,
            outlineColor: cs.outlineColor,
            outlineOffset: cs.outlineOffset,
            borderColor: cs.borderTopColor,
            backgrounds,
            what: `${el.tagName.toLowerCase()}${el.id ? `#${el.id}` : ""}${cls}`,
          });
          return;
        }
        requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
    });
  }, selector);
}

/** Parses a computed colour string. Chromium serialises every computed
 * `<color>` as `rgb(r, g, b)` or `rgba(r, g, b, a)`, so the numbers are taken
 * positionally; anything else is a change in the platform rather than in this
 * page, and throwing names it instead of silently scoring it as black. */
export function parseColor(css: string): Rgba {
  const nums = css.match(/-?[\d.]+/g);
  if (!nums || nums.length < 3) throw new Error(`unparseable computed colour: ${css}`);
  return {
    r: Number(nums[0]),
    g: Number(nums[1]),
    b: Number(nums[2]),
    a: nums.length > 3 ? Number(nums[3]) : 1,
  };
}

/** `fg` painted over `bg`, source-over. */
export function over(fg: Rgba, bg: Rgba): Rgba {
  const a = fg.a + bg.a * (1 - fg.a);
  if (a === 0) return { r: 0, g: 0, b: 0, a: 0 };
  const mix = (f: number, b: number) => (f * fg.a + b * bg.a * (1 - fg.a)) / a;
  return { r: mix(fg.r, bg.r), g: mix(fg.g, bg.g), b: mix(fg.b, bg.b), a };
}

/** What is actually painted behind a ring: the ancestor chain composited
 * from the canvas up.
 *
 * Starts on opaque white because that is the initial canvas colour when
 * nothing in the chain is opaque. In this page `body` always is, so the seed
 * never decides an assertion -- it is there so a page that changed that fact
 * would still produce a defined number rather than a transparent one. */
export function backdropOf(backgrounds: string[]): Rgba {
  let base: Rgba = { r: 255, g: 255, b: 255, a: 1 };
  for (let i = backgrounds.length - 1; i >= 0; i--) {
    base = over(parseColor(backgrounds[i]), base);
  }
  return base;
}

/** WCAG 2.x relative luminance. */
export function relativeLuminance({ r, g, b }: Rgba): number {
  const lin = (v: number) => {
    const c = v / 255;
    return c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
  };
  return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
}

/** WCAG 2.x contrast ratio between two opaque colours. */
export function contrastRatio(a: Rgba, b: Rgba): number {
  const la = relativeLuminance(a);
  const lb = relativeLuminance(b);
  return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
}

export function show({ r, g, b, a }: Rgba): string {
  return a === 1 ? `rgb(${r}, ${g}, ${b})` : `rgba(${r}, ${g}, ${b}, ${a})`;
}

/** WCAG SC 1.4.11's floor for a non-text indicator. */
export const MIN_CONTRAST = 3;
/** WCAG SC 2.4.13's floor for indicator thickness, in CSS pixels. */
export const MIN_OUTLINE_PX = 2;

/**
 * Which ancestor chain(s) a ring's own backdrop should be composited from,
 * given where the ring sits relative to the border box it outlines --
 * derived from the computed `outline-offset`/`outline-width` rather than
 * assumed for every ring (SH-360).
 *
 * `outline-offset` places the ring `offset` CSS px out from the border-box
 * edge, `outline-width` px thick, so the ring occupies the band
 * `[offset, offset + width]`. A ring entirely OUTSIDE that box
 * (`offset >= 0`, e.g. `.card`'s `+1px`) paints over whatever the PARENT
 * chain shows -- the element's own background sits inside the ring, not
 * behind it, so scoring it (as this file did before SH-360) reads the wrong
 * pixels. A ring entirely INSIDE (`offset + width <= 0`, every other ring in
 * this sheet today) paints over the element's own chain. A ring that
 * straddles the edge -- possible in principle, no rule in this sheet does it
 * today -- paints over both, and both are returned so the caller can report
 * whichever the ring clears least.
 */
export function ringBackdrops(p: IndicatorProbe): { label: string; backdrop: Rgba }[] {
  const offset = parseFloat(p.outlineOffset);
  const width = parseFloat(p.outlineWidth);
  const paintsInside = offset + width <= 0;
  const paintsOutside = offset >= 0;
  const candidates: { label: string; backdrop: Rgba }[] = [];
  if (paintsInside) {
    candidates.push({ label: "its own", backdrop: backdropOf(p.backgrounds) });
  }
  if (paintsOutside) {
    candidates.push({ label: "its parent's", backdrop: backdropOf(p.backgrounds.slice(1)) });
  }
  return candidates;
}

/** The four ways this page can resolve its palette.
 *
 * The `[data-theme]` pair deliberately sets the media query to the *opposite*
 * scheme: without that, an attribute measurement could be reading the media
 * block's copy of the tokens and nobody would know. */
export const THEMES: { name: string; apply: (page: Page) => Promise<void> }[] = [
  {
    name: "prefers-color-scheme: light",
    apply: async (page) => {
      await page.emulateMedia({ colorScheme: "light" });
      await page.evaluate(() => document.documentElement.removeAttribute("data-theme"));
    },
  },
  {
    name: "prefers-color-scheme: dark",
    apply: async (page) => {
      await page.emulateMedia({ colorScheme: "dark" });
      await page.evaluate(() => document.documentElement.removeAttribute("data-theme"));
    },
  },
  {
    name: 'data-theme="light" over a dark media query',
    apply: async (page) => {
      await page.emulateMedia({ colorScheme: "dark" });
      await page.evaluate(() => document.documentElement.setAttribute("data-theme", "light"));
    },
  },
  {
    name: 'data-theme="dark" over a light media query',
    apply: async (page) => {
      await page.emulateMedia({ colorScheme: "light" });
      await page.evaluate(() => document.documentElement.setAttribute("data-theme", "dark"));
    },
  },
];

/** Walks focus onto `selector` with real Tab presses, starting from `from`.
 *
 * Tab rather than `.focus()`, and it is not fussiness: `:focus-visible` is a
 * statement about *how* focus arrived, so a programmatic focus can leave the
 * ring undrawn on exactly the control being measured. The seed focus on
 * `from` is programmatic, but nothing is measured there -- the first Tab
 * after it is what establishes keyboard modality for everything that
 * follows. */
export async function tabOnto(page: Page, from: string, selector: string): Promise<void> {
  await page.locator(from).focus();
  for (let i = 0; i < 20; i++) {
    await page.keyboard.press("Tab");
    const there = await page.evaluate(
      (sel) => !!(document.activeElement as HTMLElement | null)?.matches(sel),
      selector,
    );
    if (there) return;
  }
  const stuck = await page.evaluate(() => {
    const el = document.activeElement as HTMLElement | null;
    return el ? `${el.tagName.toLowerCase()}${el.id ? `#${el.id}` : ""}` : "nothing";
  });
  throw new Error(`20 Tabs from ${from} never reached ${selector}; focus is on ${stuck}`);
}

/**
 * Measures a control's focus indicator, classifying it from the FOCUSED
 * probe's own `outline-style` rather than assuming every focus rule is an
 * outline (SH-360): three of the nine focus rules in `web_dashboard.html`
 * declare `outline: none` and swap `border-color` (a 1px border) instead.
 * One call name means a control that later changes kind needs no spec edit
 * -- the coverage fence (`tests/dashboard_focus_coverage.rs`) stays
 * per-control, not per-kind.
 *
 * `selector` must end in `:focus` or `:focus-visible` and name the FOCUSED
 * element -- SH-338's own convention (`":focus"` is deliberate: it asks the
 * page "whatever holds focus" rather than restating an implementation's own
 * identity, which matters for the inheritance tests in
 * `notice-focus-indicator.spec.ts`). It must also be a STRING LITERAL:
 * `tests/dashboard_focus_coverage.rs` reads this argument statically, at
 * every call site across `e2e/specs/*.spec.ts`, to prove every declared
 * focus rule in the sheet has a call site naming it -- a loop variable would
 * be invisible to that scan, silently exempting whatever it iterated over.
 *
 * `reach` performs the real interaction (Tab presses, or an Enter that opens
 * an editor) that focuses the control -- called exactly ONCE, not once per
 * theme. `.drawer-title` and `.description-field` both commit their value ON
 * BLUR, so a blur-probe-refocus cycle per theme would fire a save per theme
 * and corrupt the very state under test. The REST probe is therefore taken
 * in every theme BEFORE `reach()`, and the FOCUSED probe in every theme
 * after, both from the same held focus -- themes are re-reads, not
 * re-reaches, the same insight that makes SH-338's four-theme loop cheap.
 *
 * `.description-field`'s rest probe is taken while it is `display: none`
 * (the read/edit swap keeps both nodes in the DOM, toggling which is
 * shown) -- `getComputedStyle` still resolves colours for a hidden element,
 * so this works, but it is worth knowing before wondering why.
 */
export async function measureFocusIndicator(
  page: Page,
  selector: string,
  where: string,
  reach: () => Promise<void>,
): Promise<void> {
  const baseSelector = selector.replace(/:focus(-visible)?$/, "");
  if (baseSelector === selector) {
    throw new Error(
      `measureFocusIndicator: "${selector}" does not end in :focus or :focus-visible -- ` +
        "it must name the FOCUSED element, the convention every call site in this suite uses",
    );
  }

  const rest = new Map<string, IndicatorProbe>();
  for (const theme of THEMES) {
    await theme.apply(page);
    const probe = await probeIndicator(page, baseSelector);
    expect(probe, `${where}: nothing matches ${baseSelector} before focus, under ${theme.name}`).not.toBeNull();
    rest.set(theme.name, probe!);
  }

  await reach();

  for (const theme of THEMES) {
    await theme.apply(page);
    const focused = await probeIndicator(page, selector);
    expect(focused, `${where}: nothing matches ${selector} under ${theme.name}`).not.toBeNull();
    const f = focused!;

    expect.soft(
      f.focusVisible,
      `${where}: ${f.what} holds focus under ${theme.name} but does not match :focus-visible, ` +
        "so no indicator is drawn at all",
    ).toBe(true);

    if (f.outlineStyle !== "none" && f.outlineStyle !== "hidden") {
      // Outline kind -- SH-338's original assertion set, unchanged: an
      // author-declared ring at least MIN_OUTLINE_PX thick, clearing
      // MIN_CONTRAST against the backdrop it actually paints over.
      expect.soft(
        ["none", "hidden", "auto"],
        `${where}: ${f.what} draws outline-style: ${f.outlineStyle} under ${theme.name}. ` +
          `"auto" is the user agent's own ring, which declares no colour this page can measure -- ` +
          `its contrast is unknown rather than merely unstated, which is the defect SH-338 was filed for. ` +
          `"none"/"hidden" is no ring. Declare one, as .notice-scroll does.`,
      ).not.toContain(f.outlineStyle);

      const width = parseFloat(f.outlineWidth);
      expect.soft(
        width,
        `${where}: ${f.what}'s ring is ${f.outlineWidth} under ${theme.name}; ` +
          `SC 2.4.13 wants at least ${MIN_OUTLINE_PX} CSS px`,
      ).toBeGreaterThanOrEqual(MIN_OUTLINE_PX);

      // Skipped when there is no author colour to composite: "auto" is
      // already reported above, and scoring the user agent's placeholder
      // colour would add a second, misleading number to that report.
      if (f.outlineStyle === "auto") continue;

      let worstRatio = Infinity;
      let worstInk: Rgba = { r: 0, g: 0, b: 0, a: 1 };
      let worstBackdrop: Rgba = { r: 0, g: 0, b: 0, a: 1 };
      let worstLabel = "its own";
      for (const candidate of ringBackdrops(f)) {
        const ink = over(parseColor(f.outlineColor), candidate.backdrop);
        const ratio = contrastRatio(ink, candidate.backdrop);
        if (ratio < worstRatio) {
          worstRatio = ratio;
          worstInk = ink;
          worstBackdrop = candidate.backdrop;
          worstLabel = candidate.label;
        }
      }
      expect.soft(
        worstRatio,
        `${where}: ${f.what}'s ring is ${show(worstInk)} on ${show(worstBackdrop)} (${worstLabel} background) ` +
          `under ${theme.name} -- ${worstRatio.toFixed(2)}:1, below SC 1.4.11's ${MIN_CONTRAST}:1`,
      ).toBeGreaterThanOrEqual(MIN_CONTRAST);
      continue;
    }

    // Border-swap kind: the indicator is whatever CHANGED between rest and
    // focus (border-color or background-color), not a ring.
    const r = rest.get(theme.name)!;

    // The element's own background, COMPOSITED over its parent chain --
    // `.search-input`/`.drawer-title` rest at `background: transparent` (and
    // `.drawer-title`/`.search-input` also rest at `border: 1px solid
    // transparent`), so comparing raw parsed colours directly would score a
    // transparent rest value as literal opaque BLACK (relativeLuminance()
    // ignores alpha entirely) -- caught by a probe run that printed an
    // 18.07:1 "background change" for `.drawer-title`, an order of
    // magnitude past every real number in this sheet, and enough on its own
    // to make an all-but-invisible background swap outrank the real,
    // opaque-to-opaque `--accent` border change it sits beside. A border
    // painted over a transparent background is likewise composited onto
    // that same effective fill (`background-clip`'s default is
    // `border-box`, so the element's own background already extends under
    // the border band) before it is compared.
    const effectiveFill = (p: IndicatorProbe): Rgba =>
      over(parseColor(p.backgrounds[0]), backdropOf(p.backgrounds.slice(1)));

    const restBgDisplayed = effectiveFill(r);
    const focusBgDisplayed = effectiveFill(f);
    const restBorderDisplayed = over(parseColor(r.borderColor), restBgDisplayed);
    const focusBorderDisplayed = over(parseColor(f.borderColor), focusBgDisplayed);
    const borderChangeRatio = contrastRatio(focusBorderDisplayed, restBorderDisplayed);
    const bgChangeRatio = contrastRatio(focusBgDisplayed, restBgDisplayed);

    // SC 2.4.13's change-ratio clause, RECORDED rather than asserted --
    // weakened to "something changed" (ratio > 1) purely as a guard against
    // a vacuous pass (a control whose border is already --accent at rest,
    // say). The story explicitly declines to gate this trio's THICKNESS
    // clause (also SC 2.4.13, AAA); gating the change-ratio clause on the
    // same rows while declining the thickness one would be incoherent, so
    // the real floor is asserted only on the SC 1.4.11 basis below.
    expect.soft(
      Math.max(borderChangeRatio, bgChangeRatio),
      `${where}: ${f.what} focused vs rest under ${theme.name} -- ` +
        `border ${show(restBorderDisplayed)} -> ${show(focusBorderDisplayed)} (${borderChangeRatio.toFixed(2)}:1), ` +
        `background ${show(restBgDisplayed)} -> ${show(focusBgDisplayed)} (${bgChangeRatio.toFixed(2)}:1). ` +
        "Neither changed: focus changes nothing this instrument can see.",
    ).toBeGreaterThan(1);

    // SC 1.4.11, ASSERTED: whichever property changed more IS the
    // indicator (today, always the border -- deltas of 3-5:1 against
    // background deltas of ~1:1 in this sheet, so this is measured, not
    // assumed). A border indicator is judged against both adjacent
    // surfaces -- its own (now-focused) interior fill and its parent
    // chain's exterior -- because a border sits between the two. A
    // background indicator is judged against what it changed FROM at the
    // same location, which is exactly the change ratio already computed
    // above; a fresh "inside vs outside" split would be self-referential
    // for a fill that touches nothing but itself.
    if (borderChangeRatio >= bgChangeRatio) {
      const parentBackdrop = backdropOf(f.backgrounds.slice(1));
      const insideRatio = contrastRatio(focusBorderDisplayed, focusBgDisplayed);
      const outsideRatio = contrastRatio(focusBorderDisplayed, parentBackdrop);
      const worst = Math.min(insideRatio, outsideRatio);
      expect.soft(
        worst,
        `${where}: ${f.what}'s border indicator is ${show(focusBorderDisplayed)} under ${theme.name} -- ` +
          `${insideRatio.toFixed(2)}:1 against its own background ${show(focusBgDisplayed)}, ` +
          `${outsideRatio.toFixed(2)}:1 against its parent's ${show(parentBackdrop)} -- ` +
          `below SC 1.4.11's ${MIN_CONTRAST}:1`,
      ).toBeGreaterThanOrEqual(MIN_CONTRAST);
    } else {
      expect.soft(
        bgChangeRatio,
        `${where}: ${f.what}'s background indicator changed from ${show(restBgDisplayed)} to ${show(focusBgDisplayed)} ` +
          `under ${theme.name} -- ${bgChangeRatio.toFixed(2)}:1, below SC 1.4.11's ${MIN_CONTRAST}:1`,
      ).toBeGreaterThanOrEqual(MIN_CONTRAST);
    }
  }
}
