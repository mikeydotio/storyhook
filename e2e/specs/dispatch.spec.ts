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
 *   - "Alpha Project" (prefix AA) — has a checkout, story "Wire up the auth
 *     flow" (id in `DASHBOARD_ALPHA_STORY_ID`)
 *   - "Gamma Archive" (prefix GA) — `--no-attach`: no checkout on this
 *     machine, one story ("Archived idea") added purely so this file can
 *     open its drawer and confirm Dispatch is absent (AC1)
 *
 * `DASHBOARD_DISPATCH_TOKEN` is the daemon's real bearer token, read via
 * `story daemon token`, exactly as an operator would.
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
const ALPHA_CHECKOUT = requiredEnv("DASHBOARD_ALPHA_CHECKOUT");

test.beforeEach(async ({ page }) => {
  await page.goto("/");
});

test("Dispatch is absent for a story in a project with no checkout (AC1)", async ({
  page,
}) => {
  await page.locator(".repo-card-name", { hasText: "Gamma Archive" }).click();
  await page.locator(".card-title", { hasText: "Archived idea" }).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  // The footer's other actions (Delete, and Reopen once closed) are always
  // there; Dispatch specifically must not be, since Gamma has no checkout.
  await expect(
    page.locator("#drawer-footer button", { hasText: "Dispatch" }),
  ).toHaveCount(0);
  await expect(
    page.locator("#drawer-footer button", { hasText: "Delete" }),
  ).toBeVisible();
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

  const dispatchButton = page.locator("#drawer-footer button", {
    hasText: "Dispatch",
  });
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

  const dispatchButton = page.locator("#drawer-footer button", {
    hasText: "Dispatch",
  });
  await dispatchButton.click();

  // No modal this time -- the saved token goes straight onto the request.
  await expect(page.locator("#token-modal")).not.toHaveClass(/open/);
  await expect(dispatchButton).toBeDisabled();

  const toast = page.locator("#toast-stack .toast.success");
  await expect(toast).toBeVisible({ timeout: 20_000 });
});
