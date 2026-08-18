import { test, expect } from "./support";
import { cleanUpCreatedStories, openProject, requiredEnv } from "./support";

const DASHBOARD_NAMED_TOKEN = requiredEnv("DASHBOARD_NAMED_TOKEN");

/**
 * SH-255's acceptance criterion, asserted the only way it can honestly be
 * asserted: in a real browser, with no credential anywhere.
 *
 * This file replaces `loopback-no-token.spec.ts`, which pinned the opposite
 * property -- SH-250's tokenless loopback read exemption. SH-255 deleted
 * that exemption outright: there is no distinction left between a read and
 * a write, or between loopback and the tailnet listener. Every `/api/**`
 * request needs a token, full stop.
 *
 * **This is still the one spec in the suite that deliberately does NOT call
 * `seedToken`.** Every other spec seeds a named token into the browser
 * context's cookie jar before its first navigation, because every request
 * needs one now -- so none of them can notice if the dashboard went back to
 * rendering without a credential. That regression would be invisible to the
 * whole rest of the suite.
 *
 * Playwright's `baseURL` is loopback (`scripts/run-e2e.sh` exports
 * `DASHBOARD_URL`), so navigating here sends `Host: 127.0.0.1:<port>` --
 * still the address the deleted exemption used to treat specially. Asserting
 * against it is what proves the daemon draws no distinction any more, rather
 * than merely never having tested loopback in the first place.
 */

/**
 * Registered because the second test below creates a real story once it
 * authenticates -- unlike its predecessor in `loopback-no-token.spec.ts`,
 * which never got that far. `tests/e2e_fixture_hygiene.rs` requires this of
 * any spec that clicks `#create-submit`.
 */
cleanUpCreatedStories("Alpha Project");

test("a fresh tab at 127.0.0.1 is refused until it presents a token", async ({
  page,
}) => {
  await page.goto("/");

  // `bootstrap()`'s own first call is `fetchReposOnce()` -- GET /api/repos --
  // which 401s with no credential anywhere, on any listener now. Its 401
  // handler is what opens the modal, before this page has rendered a single
  // real project.
  await expect(page.locator("#token-modal")).toHaveClass(/open/);
  await expect(
    page.locator(".repo-card-name", { hasText: "Alpha Project" }),
  ).toHaveCount(0);
});

/**
 * The other half of the same criterion: once this tab presents a token, it
 * authenticates everything -- the read that renders the project, and the
 * write below -- with no second prompt. This is what stops the test above
 * from being satisfied by an admission gate that had simply stopped
 * answering anything at all.
 */
test("once authenticated, that same tab's first write does not prompt again", async ({
  page,
}) => {
  await page.goto("/");
  await expect(page.locator("#token-modal")).toHaveClass(/open/);
  await page.locator("#token-input").fill(DASHBOARD_NAMED_TOKEN);
  await page.locator("#token-submit").click();
  await expect(page.locator("#token-modal")).not.toHaveClass(/open/);

  await openProject(page, "Alpha Project");

  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page
    .locator("#create-title")
    .fill("SH-255 — created by a tab that authenticated once");
  await page.locator("#create-submit").click();

  await expect(
    page.locator(".card", {
      hasText: "SH-255 — created by a tab that authenticated once",
    }),
  ).toBeVisible();
  await expect(page.locator("#token-modal")).not.toHaveClass(/open/);
});
