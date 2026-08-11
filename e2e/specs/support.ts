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
