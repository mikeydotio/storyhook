import { existsSync } from "node:fs";
import { join } from "node:path";
import { test, expect } from "@playwright/test";

/**
 * Exercises the dashboard's Dispatch button (SH-50) against a real daemon
 * and a real `plugin/claude-code/bin/story.sh`, with only tmux doubled
 * (`scripts/run-e2e.sh` puts the plugin test harness's fake tmux ahead of
 * the real one on the daemon's `PATH`) — the worktree, the branch, the CAS
 * claim and the readiness/prompt handoff all run for real. This is the one
 * place in the suite that proves the daemon's dispatch endpoint actually
 * reaches production code, not just its own HTTP contract (that is
 * `tests/dispatch_endpoint.rs`'s job, against a stub script).
 *
 * Fixtures, from `scripts/run-e2e.sh`:
 *
 *   - "Alpha Project" (prefix AA) — has a checkout, three stories: "Wire up
 *     the auth flow" (id in `DASHBOARD_ALPHA_STORY_ID`, plain Dispatch),
 *     "Fix the flaky upload test" (the saved-token test), and "Roll out the
 *     new onboarding flow" (id in `DASHBOARD_ALPHA_AUTO_STORY_ID`, Dispatch
 *     Auto — SH-208) — three because each dispatches for real and claims
 *     the story, so no two tests can share one.
 *   - "Gamma Archive" (prefix GA) — `--no-attach`: no checkout on this
 *     machine, one story ("Archived idea") added purely so this file can
 *     open its drawer and confirm Dispatch is absent (AC1)
 *
 * `DASHBOARD_DISPATCH_TOKEN` is the daemon's real bearer token, read via
 * `story daemon token`, exactly as an operator would.
 *
 * Dispatch Auto's own test does not (and cannot, without a real `claude`
 * binary standing behind the fixture's fake tmux) prove the autonomous
 * charter itself runs — that the prompt text and dispatch argv differ under
 * `--auto` is `plugin/claude-code/tests/test-dispatch-auto.sh`'s job, and
 * that a closed story's self-reap works is `test-reap.sh`'s. What this file
 * proves is the one thing only a browser can: the button reaches the real
 * endpoint with `?auto=1`, the real `story.sh` really runs, and a real
 * worktree lands on disk.
 */

function requiredEnv(name: string): string {
  const value = process.env[name];
  if (!value) {
    throw new Error(
      `${name} is not set — run this suite through scripts/run-e2e.sh, which starts an ` +
        "isolated daemon, seeds the fixtures and exports the dispatch-specific variables " +
        "this spec needs.",
    );
  }
  return value;
}

const DISPATCH_TOKEN = requiredEnv("DASHBOARD_DISPATCH_TOKEN");
const ALPHA_STORY_ID = requiredEnv("DASHBOARD_ALPHA_STORY_ID");
const ALPHA_AUTO_STORY_ID = requiredEnv("DASHBOARD_ALPHA_AUTO_STORY_ID");
const ALPHA_CHECKOUT = requiredEnv("DASHBOARD_ALPHA_CHECKOUT");

test.beforeEach(async ({ page }) => {
  await page.goto("/");
});

test("Dispatch and Dispatch Auto are both absent for a story in a project with no checkout (AC1)", async ({
  page,
}) => {
  await page.locator(".repo-card-name", { hasText: "Gamma Archive" }).click();
  await page.locator(".card-title", { hasText: "Archived idea" }).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  // The footer's other actions (Delete, and Reopen once closed) are always
  // there; the two dispatch buttons specifically must not be, since Gamma
  // has no checkout.
  await expect(page.locator("#dispatch-btn")).toHaveCount(0);
  await expect(page.locator("#dispatch-auto-btn")).toHaveCount(0);
  await expect(
    page.locator("#drawer-footer button", { hasText: "Delete" }),
  ).toBeVisible();
});

test("Dispatch sits at the leading edge, before Delete", async ({ page }) => {
  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await page
    .locator(".card-title", { hasText: "Wire up the auth flow" })
    .click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  // SH-208: Dispatch, then Dispatch Auto, then Delete pushed to the
  // trailing edge by its own margin-left:auto -- DOM order is append
  // order, so this is the real, load-bearing assertion for "leading edge".
  const footerButtons = page.locator("#drawer-footer button");
  await expect(footerButtons).toHaveCount(3);
  await expect(footerButtons.nth(0)).toHaveId("dispatch-btn");
  await expect(footerButtons.nth(1)).toHaveId("dispatch-auto-btn");
  await expect(footerButtons.nth(2)).toHaveText("Delete");
});

test("Dispatch prompts for the daemon token, then runs a real dispatch (AC2)", async ({
  page,
}) => {
  test.setTimeout(45_000);

  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await page
    .locator(".card-title", { hasText: "Wire up the auth flow" })
    .click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  const dispatchButton = page.locator("#dispatch-btn");
  await expect(dispatchButton).toBeVisible();
  await dispatchButton.click();

  // No token saved yet in this fresh browser context -- the modal opens
  // rather than the request going out.
  await expect(page.locator("#token-modal")).toHaveClass(/open/);
  await page.locator("#token-input").fill(DISPATCH_TOKEN);
  await page.locator("#token-submit").click();
  await expect(page.locator("#token-modal")).not.toHaveClass(/open/);

  // The button goes non-interactive immediately (the POST's 202 lands well
  // under a second); the toast lands only after at least one 5s poll cycle.
  await expect(dispatchButton).toBeDisabled();
  await expect(dispatchButton).toHaveText("Dispatching…");
  // The OTHER button disables too (the daemon dedupes a second POST by
  // story, not by mode), but keeps its own idle label -- nothing is
  // actually dispatching autonomously.
  await expect(page.locator("#dispatch-auto-btn")).toBeDisabled();
  await expect(page.locator("#dispatch-auto-btn")).toHaveText("Dispatch Auto");

  const toast = page.locator("#toast-stack .toast.success");
  await expect(toast).toBeVisible({ timeout: 20_000 });
  await expect(toast).toContainText(ALPHA_STORY_ID);

  // The button returns to its normal, clickable state once the poll
  // resolves.
  await expect(dispatchButton).toBeEnabled();
  await expect(dispatchButton).toHaveText("Dispatch");

  // The real side effect: story.sh actually created the worktree, via the
  // same script and the same git commands the CLI's own `/story do` uses.
  const worktreePath = join(ALPHA_CHECKOUT, ".claude/worktrees", ALPHA_STORY_ID);
  expect(
    existsSync(worktreePath),
    `expected a real worktree at ${worktreePath}`,
  ).toBe(true);
});

test("Dispatch Auto sends ?auto=1 and runs a real autonomous dispatch (SH-208)", async ({
  page,
}) => {
  test.setTimeout(45_000);

  // Seeded directly rather than driven through the token modal again --
  // that flow is AC2's own test; this one is scoped to Dispatch Auto's own
  // behavior. sessionStorage survives the reload below.
  await page.evaluate(
    (token) => window.sessionStorage.setItem("storyhookDispatchToken", token),
    DISPATCH_TOKEN,
  );
  await page.reload();

  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await page
    .locator(".card-title", { hasText: "Roll out the new onboarding flow" })
    .click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  const dispatchAutoButton = page.locator("#dispatch-auto-btn");
  await expect(dispatchAutoButton).toBeVisible();

  const dispatchRequest = page.waitForRequest(
    (req) => req.method() === "POST" && req.url().includes("/dispatch?auto=1"),
  );
  await dispatchAutoButton.click();
  await dispatchRequest;

  await expect(dispatchAutoButton).toBeDisabled();
  await expect(dispatchAutoButton).toHaveText("Dispatching (auto)…");
  // The plain button disables too, but stays labeled for its own mode.
  await expect(page.locator("#dispatch-btn")).toBeDisabled();
  await expect(page.locator("#dispatch-btn")).toHaveText("Dispatch");

  const toast = page.locator("#toast-stack .toast.success");
  await expect(toast).toBeVisible({ timeout: 20_000 });
  await expect(toast).toContainText(ALPHA_AUTO_STORY_ID);
  // story.sh's own auto_note names the session autonomous in `display`,
  // relayed verbatim into the toast -- the one place this spec can observe
  // the daemon actually forwarded `--auto` all the way to the script.
  await expect(toast).toContainText(/utonomous/);

  await expect(dispatchAutoButton).toBeEnabled();
  await expect(dispatchAutoButton).toHaveText("Dispatch Auto");

  const worktreePath = join(
    ALPHA_CHECKOUT,
    ".claude/worktrees",
    ALPHA_AUTO_STORY_ID,
  );
  expect(
    existsSync(worktreePath),
    `expected a real worktree at ${worktreePath}`,
  ).toBe(true);
});

test("a saved token is not asked for again on a second dispatch", async ({
  page,
}) => {
  test.setTimeout(45_000);

  // Dispatching Alpha's other story reuses a token already in this tab's
  // sessionStorage -- Playwright starts each test with a fresh context, so
  // this seeds it directly rather than depending on the previous test
  // having run first. sessionStorage survives a same-origin reload.
  await page.evaluate(
    (token) => window.sessionStorage.setItem("storyhookDispatchToken", token),
    DISPATCH_TOKEN,
  );
  await page.reload();

  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await page
    .locator(".card-title", { hasText: "Fix the flaky upload test" })
    .click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  const dispatchButton = page.locator("#dispatch-btn");
  await dispatchButton.click();

  // No modal this time -- the saved token goes straight onto the request.
  await expect(page.locator("#token-modal")).not.toHaveClass(/open/);
  await expect(dispatchButton).toBeDisabled();

  const toast = page.locator("#toast-stack .toast.success");
  await expect(toast).toBeVisible({ timeout: 20_000 });
});
