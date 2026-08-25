import { test, expect } from "./support";
import {
  cleanUpCreatedStories,
  heldReadDeadlineMs,
  holdUntilRefused,
  openStatusesEditor,
  projectSlug,
  requiredEnv,
  seedToken,
} from "./support";

/**
 * SH-301: `state.repos` initialises `[]`, and three surfaces read that empty
 * array as an *answer* rather than as "nothing has been asked yet" --
 * `renderHome()`'s "No projects yet." (with an "Add a project" call to
 * action), `projselLabelText()`'s identical claim in the header selector,
 * and `updateSubtitle()`'s "0 projects". A fourth, `renderStatuses()`, paints
 * an unqualified "Loading…" that never leaves if the statuses fetch never
 * succeeds -- SH-291's own defect, one fetch over.
 *
 * These are the *stronger* form of SH-291's bug: an unearned negative rather
 * than an unearned "loading", the exact shape SH-284 fixed one screen down.
 * A store holding 500 projects reads as an empty one, with a button offering
 * to create a fifth-hundred-and-first.
 *
 * The fix mirrors SH-291's `state.fetchSettledFor`/`markDataSettled()`/
 * `readinessNote()` machine: `state.reposReadiness` and
 * `state.statusesReadiness` (three-valued -- "unasked" | "loaded" | "failed",
 * since `repos.length === 0` is genuinely ambiguous three ways), written by
 * `markReposSettled()`/`markStatusesSettled()` at every terminal outcome of
 * their own fetch, read through the same widened `readinessNote()`.
 */

cleanUpCreatedStories("Alpha Project");

/** The headers a write needs: the bearer token every `/api/**` route wants
 * (SH-187), plus `X-Storyhook` for `mutation_guard_ok`'s CSRF check, which a
 * read does not need (`src/api/admission.rs`). */
const writeHeaders = () => ({
  "X-Storyhook": "1",
  "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
  "Content-Type": "application/json",
});

test("the catalog names its failure instead of reporting an empty store", async ({
  page,
}) => {
  // Held from before the first request: `bootstrap()`'s own `fetchReposOnce()`
  // is what this spec needs to catch in flight.
  const repos = await holdUntilRefused(
    page,
    (url) => url.pathname === "/api/repos",
  );
  await seedToken(page);
  await page.goto(`/?catalogFetchTimeoutMs=${heldReadDeadlineMs()}`);

  const empty = page.locator(".home-empty");
  const label = page.locator("#projsel-label");
  const btn = page.locator("#projsel-btn");
  const subtitle = page.locator("#subtitle");
  const drafts = page.locator("#drafts-btn");

  // Held, not yet refused: nothing has completed, so the loading line is the
  // honest one and the error would be a lie. This is the assertion a bare
  // `state.reposFetchOk` implementation fails -- it initialises `false`, so
  // it cannot tell "never asked" from "asked and refused" and would already
  // read as failed here.
  await expect(empty).toHaveText("Loading your projects…");
  await expect(empty.locator("button")).toHaveCount(0);
  await expect(label).toHaveText("Projects");
  await expect(btn).toBeDisabled();
  await expect(subtitle).toBeHidden();
  await expect(drafts).toBeVisible();
  await expect(page.locator("#drafts-btn-text")).toHaveText("Drafts");
  await drafts.click();
  await expect(page.locator("#drafts-list")).toHaveText("Loading drafts…");

  await repos.refuse();

  // And now there is something to name. Still zero call-to-action buttons --
  // offering to create a project when the store's real count is simply
  // unknown is the half of this defect that can cause a wrong action, not
  // merely a wrong reading.
  await expect(empty).toHaveText("Couldn't load your projects. Retrying…");
  await expect(empty.locator("button")).toHaveCount(0);
  await expect(label).toHaveText("Projects");
  await expect(btn).toBeDisabled();
  await expect(btn).toHaveAttribute(
    "title",
    "Couldn't load your projects. Retrying…",
  );
  await expect(subtitle).toBeHidden();
  await expect(page.locator("#drafts-list")).toHaveText(
    "Couldn't load drafts. Retrying…",
  );
});

test("a catalog that answers after a failure drops the error where it stands", async ({
  page,
  request,
}) => {
  const repos = await holdUntilRefused(
    page,
    (url) => url.pathname === "/api/repos",
  );
  await seedToken(page);
  await page.goto(`/?catalogFetchTimeoutMs=${heldReadDeadlineMs()}`);
  await repos.refuse();

  const empty = page.locator(".home-empty");
  await expect(empty).toHaveText("Couldn't load your projects. Retrying…");

  // The claim is "Retrying…", so the repair must need no reload, no gesture
  // and no re-opened tab: the store starts answering again, and the very
  // next arrival -- pushed by the write below over `/api/events`, the same
  // channel any other client's write would use -- is what actually resolves
  // it. Every matching request past this point (the SSE-driven refetch
  // included) reaches the real daemon.
  repos.restore();
  const slug = await projectSlug(request, "Alpha Project");
  const created = await request.post(
    `/api/repos/${encodeURIComponent(slug)}/story`,
    { headers: writeHeaders(), data: { title: "SH-301 catalog readiness — the nudge" } },
  );
  if (!created.ok()) {
    throw new Error(
      `POST /story for Alpha Project answered ${created.status()}: ${await created.text()}`,
    );
  }

  await expect(page.locator(".repo-card").first()).toBeVisible();
  await expect(page.locator("#subtitle")).toHaveText(/^\d+ projects?$/);
  await expect(page.locator("#projsel-btn")).toBeEnabled();
  await expect(page.locator("#projsel-btn")).toHaveAttribute("title", "");
});

test("an empty store still offers to add a project", async ({ page }) => {
  // Mocked *data*, never behaviour (CLAUDE.md's own e2e tenet): this is a
  // real 200 with a real, empty JSON array, so `fetchReposOnce()`'s parse and
  // render paths run for real. The point is the one thing the fixture daemon
  // cannot produce on demand without deleting every seeded project out from
  // under every other spec in this file -- a catalog that has genuinely
  // answered with nothing.
  await page.route(
    (url) => url.pathname === "/api/repos",
    async (route) => {
      if (route.request().method() !== "GET") {
        await route.continue();
        return;
      }
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: "[]",
      });
    },
  );
  await seedToken(page);
  await page.goto("/");

  const empty = page.locator(".home-empty");
  await expect(empty).toContainText("No projects yet.");
  await expect(
    empty.locator("button", { hasText: "Add a project" }),
  ).toBeVisible();
  await expect(page.locator("#subtitle")).toHaveText("0 projects");
  await expect(page.locator("#projsel-label")).toHaveText("No projects yet");
  await expect(page.locator("#projsel-btn")).toBeDisabled();
  await expect(page.locator("#projsel-btn")).toHaveAttribute("title", "");
  await expect(page.locator("#drafts-btn")).toBeVisible();
  await expect(page.locator("#drafts-btn-text")).toHaveText("No Drafts");
  await page.locator("#drafts-btn").click();
  await expect(page.locator("#drafts-list")).toHaveText("No drafts.");
});

test("a partial catalog keeps known drafts visible without claiming a complete count", async ({
  page,
  request,
}) => {
  const alpha = await projectSlug(request, "Alpha Project");
  const created = await request.post(
    `/api/repos/${encodeURIComponent(alpha)}/story`,
    { headers: writeHeaders(), data: { title: "Known despite another project failing", draft: true } },
  );
  if (!created.ok()) throw new Error(`draft seed answered ${created.status()}`);

  await page.route("**/api/repos", async (route) => {
    const response = await route.fetch({
      headers: {
        ...route.request().headers(),
        "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
      },
    });
    const repos = (await response.json()) as Array<Record<string, unknown>>;
    const failed = repos.find((repo) => repo.name === "Beta Project");
    if (failed) {
      delete failed.drafts;
      failed.error = "simulated unreadable project";
      failed.available = false;
    }
    await route.fulfill({ response, json: repos });
  });
  await seedToken(page);
  await page.goto("/");

  await expect(page.locator("#drafts-btn-text")).toHaveText("Drafts");
  await page.locator("#drafts-btn").click();
  await expect(page.locator("#drafts-list .drafts-row")).toContainText(
    "Known despite another project failing",
  );
  await expect(page.locator("#drafts-list")).toContainText(
    "Couldn't load some projects' drafts. Retrying…",
  );
});

test("a statuses editor whose fetch never answers names the failure", async ({
  page,
  request,
}) => {
  await seedToken(page);
  const slug = await projectSlug(request, "Alpha Project");
  const states = await holdUntilRefused(
    page,
    (url) => url.pathname === `/api/repos/${encodeURIComponent(slug)}/states`,
  );
  const deadline = heldReadDeadlineMs();
  await page.goto(
    `/?apiGetTimeoutMs=${deadline}&catalogFetchTimeoutMs=${deadline}`,
  );
  await openStatusesEditor(page, "Alpha Project");

  const empty = page.locator(".status-empty");
  await expect(page.locator(".settings-head h2")).toHaveText(
    "Statuses · Alpha Project",
  );
  await expect(empty).toHaveText("Loading this project's statuses…");

  await states.refuse();

  await expect(empty).toHaveText(
    "Couldn't load this project's statuses. Retrying…",
  );
});
