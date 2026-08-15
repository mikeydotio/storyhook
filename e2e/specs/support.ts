import { expect, test } from "@playwright/test";
import type {
  APIRequestContext,
  APIResponse,
  Locator,
  Page,
  Request,
  Route,
} from "@playwright/test";

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
 * "touched after" unrepresentable inside its own second (SH-329). The
 * "Modified" board sort compares `updated_at` strings and breaks a tie on
 * story number ascending, so two writes in one second sort by *creation*
 * order -- which is exactly what the sort under test is supposed to
 * disprove. `board-sort.spec.ts` created two stories and touched one in
 * 1.9s of wall clock, all three writes stamped `13:31:33Z`, and the board
 * dutifully returned creation order: red, on a correct board, roughly half
 * the time. Warmer machines failed it *more* often, having fitted all three
 * writes into one second more easily -- which is why three sessions read it
 * as load-related and none of them reproduced it that way.
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
 * something to copy) and returns its id. */
export async function createStory(page: Page, title: string): Promise<string> {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  await page.locator("#create-description").fill("a description, so Copy Description has something to copy");
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
