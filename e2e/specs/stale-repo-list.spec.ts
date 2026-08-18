import { test, expect } from "./support";
import type { APIRequestContext, Page } from "@playwright/test";
import type { HeldFetch } from "./support";
import {
  cleanUpCreatedStories,
  holdFetch,
  openProject,
  projectSlug,
  requiredEnv,
  seedToken,
} from "./support";
import { mkdirSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";

/**
 * SH-282: the project list is applied in the order it *arrives*, and the
 * dashboard has several requests for it in flight at once -- every SSE
 * `repo-changed` and `repos-changed` calls `fetchReposOnce()` directly, with
 * no debounce at all (`fetchData()`, one function above it, at least has
 * `scheduleDataFetch`). The daemon answers each on whichever of its eight
 * dispatcher threads is free (`src/daemon/serve.rs`), so on a loaded machine
 * a request issued *before* a write can be answered *after* one issued behind
 * it, and the older list lands last.
 *
 * The same defect SH-281 fixed for the board, in the function immediately
 * below the one it fixed, and with worse consequences than stale counts:
 * `fetchReposOnce`'s own "the currently-viewed project was deleted elsewhere"
 * branch fires on `!state.repos.some(...)`, and a list that predates the open
 * project's creation satisfies that predicate exactly. The dashboard then
 * announces that a project the user is looking at has been deleted, and
 * throws them back to Home.
 *
 * Both specs put that ordering where the test decides rather than where the
 * machine's load happens to put it. Nothing is faked: `holdFetch` takes the
 * real list from the real daemon at the moment the page asked for it, and
 * delivers those same bytes later.
 */

cleanUpCreatedStories("Alpha Project");
cleanUpCreatedStories("Beta Project");

/** One project as `GET /api/repos` reports it (`repos_json`, `src/api/rest.rs`). */
interface RepoEntry {
  id: string;
  name: string;
  summary?: { total_open: number; ready_count: number; blocked_count: number };
}

/** A fourth project, created by the spec that needs a list predating one. */
const NEW_PROJECT = { name: "Epsilon Project", prefix: "EP" };

/**
 * Holds the first project-list fetch whose body satisfies `until`.
 *
 * The path is matched exactly rather than by suffix: `/api/repos` is also the
 * prefix of every per-project route (`/api/repos/<slug>/data` and friends),
 * and holding one of those would stall an unrelated part of the page.
 */
function holdRepoList(
  page: Page,
  until: (repos: RepoEntry[]) => boolean,
): Promise<HeldFetch> {
  return holdFetch<RepoEntry[]>(
    page,
    (url) => url.pathname === "/api/repos",
    until,
  );
}

/** The headers a write needs: the bearer token every `/api/**` route wants
 * (SH-187), plus `X-Storyhook` for `mutation_guard_ok`'s CSRF check, which a
 * read does not need (`src/api/admission.rs`). */
const writeHeaders = () => ({
  "X-Storyhook": "1",
  "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
  "Content-Type": "application/json",
});

/**
 * Creates a story through the API rather than the board.
 *
 * Every spec here observes the *Home* screen, which has no create control of
 * its own, and the point of each write is only to move a project's counts and
 * make the page ask for the list again -- which every mutation does, through
 * the SSE `repo-changed` these produce.
 */
async function createStory(
  request: APIRequestContext,
  projectName: string,
  title: string,
): Promise<void> {
  const slug = await projectSlug(request, projectName);
  const created = await request.post(
    `/api/repos/${encodeURIComponent(slug)}/story`,
    { headers: writeHeaders(), data: { title } },
  );
  if (!created.ok()) {
    throw new Error(
      `createStory: POST /story for "${projectName}" answered ` +
        `${created.status()}: ${await created.text()}`,
    );
  }
}

/** Where this run's seeded checkouts live -- `run-e2e.sh`'s own `$seed_dir`,
 * inside the data root it removes on exit, reached through the one checkout
 * it exports. A project created anywhere else would outlive the run. */
const seedRoot = () => dirname(requiredEnv("DASHBOARD_ALPHA_CHECKOUT"));

/** `POST /api/repos` -- the same operation as `story project new --attach`,
 * through the same invoker (`route_init_repo`). The directory has to exist
 * first: this is "create a project at a path on this machine", and the
 * browser's own form says the same. Not a git repository, deliberately --
 * nothing here dispatches, and a project is a recorded path until something
 * reads it as a repository. */
async function createProject(request: APIRequestContext): Promise<void> {
  const path = join(seedRoot(), "sh-282-epsilon");
  mkdirSync(path, { recursive: true });
  const created = await request.post("/api/repos", {
    headers: writeHeaders(),
    data: { path, prefix: NEW_PROJECT.prefix, name: NEW_PROJECT.name },
  });
  if (!created.ok()) {
    throw new Error(
      `createProject: POST /api/repos answered ${created.status()}: ` +
        `${await created.text()}`,
    );
  }
}

/**
 * Deletes the project the spec above created, if it got that far, and the
 * directory behind it.
 *
 * Unconditional and in an `afterEach` for the same reason
 * `cleanUpCreatedStories` is (SH-245): a failure anywhere above strands it,
 * and a stranded project is a fifth Home card, a fifth row in the settings
 * table and a fifth entry in the selector for every spec that runs after this
 * file -- including `#subtitle`'s own "N projects".
 */
test.afterEach(async ({ request }) => {
  const path = join(seedRoot(), "sh-282-epsilon");
  const listed = await request.get("/api/repos", {
    headers: { "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN") },
  });
  const repos: RepoEntry[] = await listed.json();
  const match = repos.find((r) => r.name === NEW_PROJECT.name);
  if (match) {
    // `confirm` naming the slug is the second of `route_delete_repo`'s two
    // steps -- without it a project holding anything answers 409 with the
    // plan rather than deleting, and this cleanup would quietly leave it.
    const deleted = await request.delete(
      `/api/repos/${encodeURIComponent(match.id)}`,
      { headers: writeHeaders(), data: { confirm: match.id } },
    );
    if (!deleted.ok()) {
      throw new Error(
        `DELETE ${match.id} answered ${deleted.status()}: ${await deleted.text()}`,
      );
    }
  }
  rmSync(path, { recursive: true, force: true });
});

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  // Home, where the per-project counts are rendered, and where a project
  // evicted from its own board lands.
  await expect(page.locator(".repo-card-name").first()).toBeVisible();
  // Every write below reaches the page as an SSE event, so a spec that wrote
  // before the connection opened would wait out its whole timeout for a fetch
  // nothing ever asked for. `onopen` is also a `fetchReposOnce()` of its own,
  // which is exactly the sort of reply a spec must not confuse for the one it
  // arranged -- so it is spent before either test arms anything.
  await expect(page.locator("#conn-text")).toHaveText("Live");
});

/** `name`'s open-story count, as its Home card renders it. */
function openCount(page: Page, name: string) {
  return page
    .locator(".repo-card", { hasText: name })
    .locator(".repo-card-stats span")
    .first();
}

test("a project list taken before a story existed must not un-render the count that story produced", async ({
  page,
  request,
}) => {
  const alphaOpen = openCount(page, "Alpha Project");
  await expect(alphaOpen).toHaveText(/^\d+ open$/);
  const before = Number((await alphaOpen.textContent())!.split(" ")[0]);

  // The list as it is now, held back: Alpha's count is still the old one, and
  // this reply is about to be overtaken by a newer one.
  const stale = await holdRepoList(page, (repos) =>
    repos.some(
      (r) => r.name === "Alpha Project" && r.summary?.total_open === before,
    ),
  );
  // A write in *another* project, purely to make the page ask for the list:
  // it is answered with Alpha unchanged, which is the snapshot this spec
  // needs, and nothing else would issue a request at a moment the test
  // controls.
  await createStory(request, "Beta Project", "SH-282 stale list — the nudge");
  await stale.taken;

  // The write the held list predates, and the newer reply that carries it.
  await createStory(
    request,
    "Alpha Project",
    "SH-282 stale list — the story the held list has never heard of",
  );
  await expect(alphaOpen).toHaveText(`${before + 1} open`);

  await stale.seal();
  await stale.deliver();

  await expect(alphaOpen).toHaveText(`${before + 1} open`);
});

test("a project list taken before a project existed must not evict the user from that project", async ({
  page,
  request,
}) => {
  // A list from before the project below is created -- so one whose
  // `state.repos` does not contain it, which is what
  // `fetchReposOnce`'s deleted-elsewhere branch reads as "deleted".
  const stale = await holdRepoList(
    page,
    (repos) => !repos.some((r) => r.name === NEW_PROJECT.name),
  );
  await createStory(request, "Beta Project", "SH-282 eviction — the nudge");
  await stale.taken;

  await createProject(request);
  const card = page.locator(".repo-card", { hasText: NEW_PROJECT.name });
  // The newer list has been applied: the project the user is about to open is
  // on screen, and the held one is now strictly older than what the page has
  // already seen.
  await expect(card).toBeVisible();

  // Nothing lands from here but the held list itself -- in production the
  // fetches that would repair this have already been spent, and the next is a
  // safety poll 25 seconds out.
  await stale.seal();
  await openProject(page, NEW_PROJECT.name);
  await expect(page.locator("#projsel-btn")).toContainText(
    `${NEW_PROJECT.prefix} · ${NEW_PROJECT.name}`,
  );

  await stale.deliver();

  // Asserted before the screen, because this is the claim the user actually
  // reads, and an error toast lives longer than the render that produced it
  // (`TOAST_ERROR_LIFETIME_MS`) -- so a failure here names the defect rather
  // than the navigation it caused.
  await expect(
    page.locator(".toast", { hasText: "This project was deleted" }),
  ).toHaveCount(0);
  await expect(page.locator("#board-view")).toBeVisible();
  await expect(page.locator("#projsel-btn")).toContainText(
    `${NEW_PROJECT.prefix} · ${NEW_PROJECT.name}`,
  );
});
