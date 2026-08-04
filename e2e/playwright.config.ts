import { defineConfig, devices } from "@playwright/test";

/**
 * Config for the dashboard's browser suite.
 *
 * There is no `webServer` block: `scripts/run-e2e.sh` starts a real `story`
 * daemon (the actual compiled binary, isolated storage) before invoking
 * Playwright, and stops it after — the daemon's readiness and the seeded
 * projects it serves are the harness's job, not this file's. `DASHBOARD_URL`
 * is required rather than defaulted so a spec can never silently run against
 * a developer's real dashboard on :3456.
 */
const baseURL = process.env.DASHBOARD_URL;
if (!baseURL) {
  throw new Error(
    "DASHBOARD_URL is not set — run this suite through scripts/run-e2e.sh, " +
      "which starts an isolated daemon and points specs at it. Running " +
      "`npx playwright test` directly would otherwise default to nothing " +
      "and fail every spec with a connection error.",
  );
}

export default defineConfig({
  testDir: "./specs",
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: 0,
  workers: 1,
  reporter: [["list"]],
  timeout: 15_000,
  expect: { timeout: 5_000 },
  use: {
    baseURL,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
  ],
});
