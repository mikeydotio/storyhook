import { expect, test } from "@playwright/test";
import type {
  APIRequestContext,
  APIResponse,
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
 * Opens the filter bar's disclosure panel (SH-235) if it isn't already
 * open, and waits for it to actually render. The panel defaults collapsed
 * -- a fresh Playwright context has no localStorage, same reasoning as
 * `seedToken`'s own comment above, so every spec that drives a control
 * inside it (a priority/assignee/type/state/columns dropdown, "Show
 * closed"/"Show archived"/"Hide empty columns", or the board-sort buttons)
 * needs this first. `#filter-count` and `#filter-clear` are in the
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
