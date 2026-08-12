import { test, expect } from "@playwright/test";
import { cleanUpCreatedStories } from "./support";

/**
 * SH-250's acceptance criterion, asserted the only way it can honestly be
 * asserted: in a real browser, with no credential anywhere.
 *
 * **This is the one spec in the suite that deliberately does NOT call
 * `seedToken`.** Every other spec seeds the daemon token into
 * `sessionStorage` before its first navigation, because writes and dispatch
 * still require it — so none of them can notice if the dashboard went back to
 * demanding a token merely to *render*. That regression would be invisible to
 * the whole rest of the suite.
 *
 * Two things have to hold together for this to pass, and they live in
 * different layers:
 *
 * 1. the daemon exempts a loopback read from the token
 *    (`api::admission::token_exempt`), and
 * 2. the page stopped prompting pre-emptively — its bootstrap used to open
 *    the token modal whenever `sessionStorage` was empty, which kept the
 *    dashboard unusable no matter what the server decided.
 *
 * Reverting either one fails this spec.
 *
 * Playwright's `baseURL` is loopback (`scripts/run-e2e.sh` exports
 * `DASHBOARD_URL`), so navigating here sends `Host: 127.0.0.1:<port>` — the
 * `Host` conjunct satisfied exactly as a person typing that URL satisfies it.
 */

/**
 * Registered even though the second test below asserts its story is *never*
 * created — and precisely because of that.
 *
 * `tests/e2e_fixture_hygiene.rs` requires this of any spec that clicks
 * `#create-submit`, and here the requirement earns its keep rather than merely
 * being satisfied: the whole claim of that test is that a tokenless write is
 * refused. If it ever stops being refused, the story *is* created, in a fixture
 * project two other specs count the cards of — so the exact regression this
 * file exists to catch is also the one that would strand a story (SH-245).
 * The sweep runs on failure as well as success, so the red is reported once,
 * here, instead of three times in specs that were never involved.
 */
cleanUpCreatedStories("Alpha Project");

test("a fresh tab at 127.0.0.1 renders without ever asking for a token", async ({
  page,
}) => {
  await page.goto("/");

  // The projects list is a read of `GET /api/repos`. If the exemption were
  // gone, this would be a 401 and the modal would be up instead.
  await expect(
    page.locator(".repo-card-name", { hasText: "Alpha Project" }),
  ).toBeVisible();

  await expect(page.locator("#token-modal")).not.toHaveClass(/open/);

  // And a project's own board, which is the second read (`/data`) and the one
  // that returns every story in the project -- the disclosure the token used
  // to be the only thing standing in front of.
  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await expect(page.locator("#board-view")).toBeVisible();
  await expect(page.locator(".column .card").first()).toBeVisible();
  await expect(page.locator("#token-modal")).not.toHaveClass(/open/);

  // Nothing put a token in storage along the way: the page really did read
  // without a credential, rather than quietly acquiring one.
  const stored = await page.evaluate(() =>
    window.sessionStorage.getItem("storyhookDaemonToken"),
  );
  expect(stored).toBeNull();
});

/**
 * The other half of the same criterion: the exemption covers reads, so the
 * first *write* still asks. This is what stops the spec above from being
 * satisfied by an admission gate that had simply stopped checking anything.
 */
test("the first write from that same tokenless tab still prompts", async ({
  page,
}) => {
  await page.goto("/");
  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await expect(page.locator("#board-view")).toBeVisible();

  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page
    .locator("#create-title")
    .fill("SH-250 — never created, the token modal intercepts this");
  await page.locator("#create-submit").click();

  // The mutation 401s, and `api()`'s own retry-on-401 path opens the modal.
  await expect(page.locator("#token-modal")).toHaveClass(/open/);
});
