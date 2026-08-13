import { expect, test } from "@playwright/test";
import type { APIRequestContext, Page } from "@playwright/test";

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
 * Seeds the daemon's bearer token into `sessionStorage` under the same key
 * `web_dashboard.html` reads (`storyhookDaemonToken`), before any of the
 * page's own scripts run.
 *
 * `addInitScript` runs on every subsequent navigation in this page's
 * context, not just the next one -- deliberately, since `dispatch.spec.ts`
 * reloads mid-test and still needs the token there. It has to be
 * registered before `page.goto()`: setting it afterward (e.g. via
 * `page.evaluate`) would race the page's own bootstrap sequence, which
 * reads the token on its very first tick.
 */
export async function seedToken(page: Page): Promise<void> {
  const token = requiredEnv("DASHBOARD_TOKEN");
  await page.addInitScript((value) => {
    window.sessionStorage.setItem("storyhookDaemonToken", value);
  }, token);
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
