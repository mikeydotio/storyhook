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

/**
 * A spec whose subject is the phone — a coarse pointer, a narrow viewport,
 * `hasTouch` — rather than the dashboard's behavior in general. One
 * pattern, referenced by both projects below, so they stay exhaustive and
 * disjoint by construction: every spec file matches it or it doesn't, and
 * no file can end up running in both (where the desktop project would fail
 * a `(pointer: coarse)` assertion by design) or in neither (where it would
 * silently run nowhere). Two independently hand-maintained globs would
 * drift the way `--test-threads`-adjacent counts have before (SH-136); a
 * single regex used twice cannot.
 */
const MOBILE_SPECS = /\.mobile\.spec\.ts$/;

export default defineConfig({
  testDir: "./specs",
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: 0,
  workers: 1,
  reporter: [["list"]],
  // Half Playwright's own 30s default, and measured rather than inherited
  // (SH-222). Three full runs of the suite, the machine loaded by spinners
  // to the range the reported failures were seen in:
  //
  //   load avg      suite    slowest test on this budget
  //   idle (~3)     2.9m     7.7s   reopen-soft-deleted-confirm.spec.ts:210
  //   ~44           4.4m     8.6s   reopen-soft-deleted-confirm.spec.ts:167
  //   ~100          6.4m    10.0s   board-sort.spec.ts:54
  //
  // 111/111 green in all three, so the worst case observed leaves a third of
  // the budget unspent at a load average of 100 — above the 32-88 SH-222
  // recorded. The three specs that drive a real dispatch are not on this
  // budget: they call `test.setTimeout()` with a multiple of their own
  // measured `DISPATCH_COMPLETION_TIMEOUT` (SH-245), because they wait on a
  // subprocess and nothing else here does.
  //
  // SH-222 was filed suspecting this number, on failures that all timed out
  // against it. None of them was this number being wrong: two were a spec
  // acting before the board had data (an action that could not succeed at
  // any budget, `openProject()` in specs/support.ts) and one was a closed
  // story stranded in a shared fixture. Raising it would have hidden both
  // for exactly as long as the next machine was slower.
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
      testIgnore: MOBILE_SPECS,
    },
    {
      // Chromium under mobile emulation, not a real phone: `make
      // e2e-install` installs chromium and nothing else, so a WebKit/iOS
      // device descriptor would fail with a missing-browser error on every
      // machine. `devices["Pixel 7"]` is `defaultBrowserType: "chromium"`
      // with `isMobile: true` and `hasTouch: true`, which is what puts
      // Blink into mobile emulation -- and mobile emulation is what makes
      // `(pointer: coarse)` match (SH-256). The engine under test is
      // therefore Blink, not WebKit, and iOS Safari's 16px zoom threshold
      // is WebKit's own rule: what this project verifies is that the
      // *mechanism* -- the dashboard's coarse-pointer CSS override --
      // fires and lands every control at or above that threshold, which is
      // the part that can regress. Only a real iPhone proves the browser
      // stays unzoomed.
      name: "mobile-chromium",
      use: { ...devices["Pixel 7"] },
      testMatch: MOBILE_SPECS,
    },
  ],
});
