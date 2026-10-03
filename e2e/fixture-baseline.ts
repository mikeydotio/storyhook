import { request } from "@playwright/test";
import type { APIRequestContext, FullConfig } from "@playwright/test";
import { fixtureApiUrl, gracedRequestBudget, requiredEnv } from "./fixture-api";

/**
 * The run's fixture baseline (SH-765): every project's story and draft IDs as
 * `scripts/run-e2e.sh` seeded them, captured by this global setup before any
 * worker exists.
 *
 * Why the run, not the worker. Playwright stops a worker after any failed
 * test and starts a fresh one for the next. The baseline used to be module
 * state in `specs/support.ts`, captured the first time a spec asked. So a
 * cleanup that failed left its stray in the project, and the next worker's
 * first capture took that stray in as "fixture". It then stayed for the rest
 * of the run, and every later test that counted the project failed on it.
 * Captured here, the baseline is pristine by construction, and one run shares
 * one baseline however many workers it starts.
 *
 * Handed to workers through the environment: Playwright forks every worker
 * with the runner's `process.env`, which this setup has already set. The
 * name is `E2E_`, not `STORYHOOK_`, because Storyhook forwards its own
 * prefix to the processes it spawns (`src/env/spawn_env.rs`), and one spec
 * restarts the daemon with the full `process.env`.
 */
export const FIXTURE_BASELINE_ENV = "E2E_FIXTURE_BASELINE";

/** One board story, as `GET .../data` reports it: open and closed alike,
 * neither deleted nor draft (`project_data_json` in `src/api/rest.rs`). */
export interface BoardStory {
  id: string;
  superstate: string;
}

/** Reads one project's board stories and drafts from `GET .../data`, by slug.
 * `label` names the project in a refusal. */
export async function projectStories(
  api: APIRequestContext,
  slug: string,
  label: string,
): Promise<BoardStory[]> {
  const resp = await api.get(
    fixtureApiUrl(`/api/repos/${encodeURIComponent(slug)}/data`),
    { timeout: gracedRequestBudget(), headers: { "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN") } },
  );
  if (!resp.ok()) {
    throw new Error(`GET /data for "${label}" answered ${resp.status()}: ${await resp.text()}`);
  }
  const data = await resp.json();
  const views = [...(data.stories ?? []), ...(data.drafts ?? [])];
  return views.map((view: { story: BoardStory }) => ({
    id: view.story.id,
    superstate: view.story.superstate,
  }));
}

/** Every project's story and draft IDs, keyed by project name, as the daemon
 * reports them now. */
export async function captureFixtureBaseline(
  api: APIRequestContext,
): Promise<Record<string, string[]>> {
  const resp = await api.get(fixtureApiUrl("/api/repos"), {
    timeout: gracedRequestBudget(),
    headers: { "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN") },
  });
  if (!resp.ok()) {
    throw new Error(`GET /api/repos answered ${resp.status()}: ${await resp.text()}`);
  }
  const repos: Array<{ id: string; name: string }> = await resp.json();
  const baseline: Record<string, string[]> = {};
  for (const repo of repos) {
    baseline[repo.name] = (await projectStories(api, repo.id, repo.name)).map((s) => s.id);
  }
  return baseline;
}

/**
 * The story and draft IDs `projectName` held when the run began. Read when a
 * hook or fixture runs, never at module load: `playwright test --list`, which
 * `scripts/run-e2e.sh` runs first, loads every spec without running any
 * global setup. Loud when the setup did not run, when its value is not the
 * shape it writes, or when the project did not exist at the start of the run.
 */
export function fixtureBaseline(projectName: string): ReadonlySet<string> {
  const raw = process.env[FIXTURE_BASELINE_ENV];
  if (!raw) {
    throw new Error(
      `${FIXTURE_BASELINE_ENV} is not set: the run's global setup (e2e/fixture-baseline.ts) ` +
        "did not capture a fixture baseline. Run this suite through scripts/run-e2e.sh.",
    );
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch (error) {
    throw new Error(`${FIXTURE_BASELINE_ENV} is not JSON: ${raw}`, { cause: error });
  }
  if (
    parsed === null || typeof parsed !== "object" || Array.isArray(parsed) ||
    !Object.values(parsed).every(
      (ids) => Array.isArray(ids) && ids.every((id) => typeof id === "string"),
    )
  ) {
    throw new Error(`${FIXTURE_BASELINE_ENV} is not a map of project names to story IDs: ${raw}`);
  }
  const baseline = parsed as Record<string, string[]>;
  if (!Object.hasOwn(baseline, projectName)) {
    throw new Error(
      `the run's fixture baseline has no project named "${projectName}"; it has ` +
        `${Object.keys(baseline).map((name) => `"${name}"`).join(", ")}`,
    );
  }
  return new Set(baseline[projectName]);
}

/** Global setup: capture the baseline and hand it to every worker. */
export default async function captureRunBaseline(_config: FullConfig): Promise<void> {
  const api = await request.newContext();
  try {
    const baseline = await captureFixtureBaseline(api);
    process.env[FIXTURE_BASELINE_ENV] = JSON.stringify(baseline);
    const counts = Object.entries(baseline).map(([name, ids]) => `${name}=${ids.length}`);
    console.error(`fixture-baseline: captured before any worker: ${counts.join(", ")}`);
  } finally {
    await api.dispose();
  }
}
