import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { test, expect } from "./support";
import { dispatchStory, openProject, requiredEnv, seedToken } from "./support";

/**
 * Exercises the dashboard's Dispatch button (SH-50) against a real daemon
 * and a real `plugins/story/bin/story.sh`, with only tmux doubled
 * (`scripts/run-e2e.sh` puts the plugin test harness's fake tmux ahead of
 * the real one on the daemon's `PATH`) — the worktree, the branch, the CAS
 * claim and the readiness/prompt handoff all run for real. This is the one
 * place in the suite that proves the daemon's dispatch endpoint actually
 * reaches production code, not just its own HTTP contract (that is
 * `tests/dispatch_endpoint.rs`'s job, against a stub script).
 *
 * Fixtures, from `scripts/run-e2e.sh`:
 *
 *   - "Alpha Project" (prefix AA) — has a checkout, two stories: "Wire up
 *     the auth flow" (id in `DASHBOARD_ALPHA_STORY_ID`, plain Dispatch) and
 *     "Fix the flaky upload test" (the saved-token test) — each dispatches
 *     for real and claims the story, so no two tests can share one.
 *   - "Delta Project" (prefix DD) — has a checkout, one story ("Roll out
 *     the new onboarding flow", id in `DASHBOARD_DELTA_STORY_ID`), reserved
 *     for the modal's Auto mode test (SH-208/SH-436). A project of its own rather
 *     than a third Alpha story: Alpha's exact two-story, four-empty-column
 *     shape is a fixture filter-persistence.spec.ts and
 *     column-visibility.spec.ts assert on byte-for-byte.
 *   - "Gamma Archive" (prefix GA) — `--no-attach`: no checkout on this
 *     machine, one story ("Archived idea") added purely so this file can
 *     open its drawer and confirm Dispatch is absent (AC1)
 *
 * `DASHBOARD_NAMED_TOKEN` (`./support.ts`) is a named token minted for the
 * suite via `story token new` (SH-255) -- what a real user pastes into the
 * modal now, and what `seedToken` seeds as a cookie for every test but the
 * one that exercises the modal itself, the same way an already-authenticated
 * browser tab would carry it. `DASHBOARD_TOKEN`, the daemon's own rotating
 * bearer token, remains what non-browser callers use; it does not validate
 * against the named-token exchange the modal now performs, so it must never
 * be the thing pasted into `#token-input`.
 *
 * Auto mode's own test does not (and cannot, without a real `claude`
 * binary standing behind the fixture's fake tmux) prove the autonomous
 * charter itself runs — that the prompt text and dispatch argv differ under
 * `--auto` is `plugins/story/tests/test-dispatch-auto.sh`'s job, and
 * that a closed story's self-reap works is `test-reap.sh`'s. What this file
 * proves is the one thing only a browser can: the button reaches the real
 * endpoint with `?auto=1`, the real `story.sh` really runs, and a real
 * worktree lands on disk.
 */

const DASHBOARD_NAMED_TOKEN = requiredEnv("DASHBOARD_NAMED_TOKEN");
const ALPHA_STORY_ID = requiredEnv("DASHBOARD_ALPHA_STORY_ID");
const ALPHA_CHECKOUT = requiredEnv("DASHBOARD_ALPHA_CHECKOUT");
const DELTA_STORY_ID = requiredEnv("DASHBOARD_DELTA_STORY_ID");
const DELTA_CHECKOUT = requiredEnv("DASHBOARD_DELTA_CHECKOUT");

/**
 * How long a real dispatch has to report completion, measured from the
 * click that starts it.
 *
 * The dashboard learns a dispatch finished from its own 5s status poll, so
 * this budget covers all of `story.sh` -- `git worktree add`, the CAS claim,
 * the readiness handoff -- plus up to one poll interval of rounding. It was
 * 20s, and SH-245 records a `make test` that ran out of it while a second
 * build had the machine. Measured at ~5.5s here with two other e2e suites
 * and a full `cargo test --no-run` compile running alongside, so 20s was
 * under 4x of headroom for work whose duration is a subprocess's, not the
 * dashboard's.
 *
 * A green run pays nothing for the larger number: the assertion resolves
 * the moment the notice appears. The only cost is a slower
 * report on a dispatch that genuinely never finishes, which the per-test
 * timeout still bounds.
 */
const DISPATCH_COMPLETION_TIMEOUT = 45_000;

// No shared `beforeEach` here, unlike this suite's other spec files: the
// "prompts for the daemon token" test below deliberately navigates with NO
// token seeded, since that is the one thing it exercises. Every other test
// seeds it itself, first thing, the same way an already-authenticated
// browser tab would carry it in.

test("Dispatch is absent for a story in a project with no checkout (AC1)", async ({
  page,
}) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Gamma Archive");
  await page.locator(".card-title", { hasText: "Archived idea" }).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  // The footer's other actions (Delete, and Reopen once closed) are always
  // there; the dispatch button specifically must not be, since Gamma
  // has no checkout.
  await expect(page.locator("#dispatch-btn")).toHaveCount(0);
  await expect(page.locator("#dispatch-auto-btn")).toHaveCount(0);
  await expect(
    page.locator("#drawer-footer button", { hasText: "Delete" }),
  ).toBeVisible();
});

test("Dispatch sits at the leading edge, before Close and Delete", async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await page
    .locator(".card-title", { hasText: "Wire up the auth flow" })
    .click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  // Dispatch, then Close and Delete at the trailing edge -- DOM order is
  // append order, so this is the real, load-bearing assertion for "leading
  // edge". Close became the first trailing action in SH-507.
  const footerButtons = page.locator("#drawer-footer button");
  await expect(footerButtons).toHaveCount(3);
  await expect(footerButtons.nth(0)).toHaveId("dispatch-btn");
  await expect(footerButtons.nth(1)).toHaveText("Close");
  await expect(footerButtons.nth(2)).toHaveText("Delete");
});

test("Dispatch opens with defaults and does not remember cancelled edits", async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await page.locator(".card-title", { hasText: "Wire up the auth flow" }).click();

  await page.locator("#dispatch-btn").click();
  const modal = page.locator("#dispatch-modal");
  const backdrop = page.locator("#dispatch-modal-backdrop");
  await expect(modal).toHaveClass(/open/);
  await expect(modal).toHaveAttribute("role", "dialog");
  await expect(page.locator("#dispatch-client")).toHaveValue("localhost");
  await expect(page.locator("#dispatch-client option")).toHaveText(["localhost"]);
  await expect(page.locator("#dispatch-agent option")).toHaveText(["Claude", "Codex"]);
  await expect(page.locator("#dispatch-agent")).toHaveValue("claude");
  await expect(page.locator("#dispatch-auto")).not.toBeChecked();
  await expect(page.locator(".dispatch-auto-help")).toHaveText(
    "Auto-approves the implementation plan, handles questions autonomously, merges PRs, and cleans up its workspace when done.",
  );

  // SH-517: Model/Effort/Speed are fetched from GET /api/dispatch-options
  // and start on "Default" -- real story.sh's own `capabilities` catalog,
  // not a fixture, since this spec (unlike dispatch_endpoint.rs's stubbed
  // suite) runs against the real helper.
  await expect(page.locator("#dispatch-model option")).toHaveText([
    "Default", "Opus+Sonnet", "Opus", "Fable", "Sonnet", "Haiku",
  ]);
  await expect(page.locator("#dispatch-effort option")).toHaveText([
    "Default", "low", "medium", "high", "xhigh", "max",
  ]);
  await expect(page.locator("#dispatch-speed option")).toHaveText(["Default", "Fast"]);
  await expect(page.locator("#dispatch-model")).toHaveValue("");
  await expect(page.locator("#dispatch-effort")).toHaveValue("");
  await expect(page.locator("#dispatch-speed")).toHaveValue("");

  // Switching Provider rebuilds Model/Effort/Speed for the NEWLY selected
  // one -- a Claude model has no meaning once Codex is chosen, and Codex's
  // own catalog is a materially different shape (an extra "none" effort,
  // no forced default model).
  await page.locator("#dispatch-agent").selectOption("codex");
  await expect(page.locator("#dispatch-model option")).toHaveText([
    "Default", "GPT-5.6 Sol", "GPT-5.6 Terra", "GPT-5.6 Luna",
  ]);
  await expect(page.locator("#dispatch-effort option")).toHaveText([
    "Default", "none", "low", "medium", "high", "xhigh", "max",
  ]);
  await expect(page.locator("#dispatch-speed option")).toHaveText(["Default", "Fast"]);

  await page.locator("#dispatch-model").selectOption("gpt-5.6-sol");
  await page.locator("#dispatch-effort").selectOption("low");
  await page.locator("#dispatch-speed").selectOption("fast");
  await page.locator("#dispatch-auto").check();
  await page.locator("#dispatch-modal-cancel").click();
  await expect(modal).not.toHaveClass(/open/);
  await expect(page.locator("#dispatch-btn")).toBeFocused();
  await expect(backdrop).toBeHidden();

  // Cancel discards the whole draft, model/effort/speed included -- the
  // reopened modal is exactly the pre-edit state above, not a mix of the
  // last submission (there has been none yet) and the cancelled edits.
  await page.locator("#dispatch-btn").click();
  await expect(page.locator("#dispatch-agent")).toHaveValue("claude");
  await expect(page.locator("#dispatch-model")).toHaveValue("");
  await expect(page.locator("#dispatch-effort")).toHaveValue("");
  await expect(page.locator("#dispatch-speed")).toHaveValue("");
  await expect(page.locator("#dispatch-auto")).not.toBeChecked();
  await backdrop.click({ position: { x: 2, y: 2 } });
  await expect(modal).not.toHaveClass(/open/);
  await expect(backdrop).toBeHidden();

  await page.locator("#dispatch-btn").click();
  await page.keyboard.press("Escape");
  await expect(modal).not.toHaveClass(/open/);
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(page.locator("#dispatch-btn")).toBeFocused();
});

test("a tab authenticates once on load, and dispatch needs no second prompt (AC2)", async ({
  page,
}) => {
  // Two dispatches to wait out here, at DISPATCH_COMPLETION_TIMEOUT apiece.
  test.setTimeout(2 * DISPATCH_COMPLETION_TIMEOUT + 30_000);

  // No token seeded. SH-255 deleted SH-250's tokenless loopback read
  // exemption, so the very first read this tab makes (`fetchReposOnce`'s
  // `GET /api/repos`, fired from `bootstrap()` before this page renders
  // anything real) 401s, and that handler's own retry-on-401 logic opens
  // the modal before the repo list ever appears -- not the Dispatch click.
  await page.goto("/");
  await expect(page.locator("#token-modal")).toHaveClass(/open/);
  await page.locator("#token-input").fill(DASHBOARD_NAMED_TOKEN);
  await page.locator("#token-submit").click();
  await expect(page.locator("#token-modal")).not.toHaveClass(/open/);

  // That one exchange is the whole story now: `/token` traded the pasted
  // value for an `HttpOnly` cookie, which the browser attaches to every
  // request in this tab automatically -- reads, and the dispatch write
  // below -- so nothing past this point prompts a second time.
  await openProject(page, "Alpha Project");
  await page
    .locator(".card-title", { hasText: "Wire up the auth flow" })
    .click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  const dispatchButton = page.locator("#dispatch-btn");
  await expect(dispatchButton).toBeVisible();
  await dispatchStory(page);

  // The "no second prompt" half of AC2: the cookie already authenticates
  // the POST, so the token modal never reopens and dispatch runs from the
  // configuration submit. The button goes non-interactive immediately (`startDispatch`
  // marks the story in-flight before the POST resolves); the toast lands
  // only after at least one 5s poll cycle.
  await expect(page.locator("#token-modal")).not.toHaveClass(/open/);
  await expect(dispatchButton).toBeDisabled();
  await expect(dispatchButton).toHaveText("Dispatching…");
  const toast = page.locator("#toast-stack .toast.success");
  await expect(toast).toBeVisible({ timeout: DISPATCH_COMPLETION_TIMEOUT });
  await expect(toast).toHaveText(`${ALPHA_STORY_ID} dispatched`);

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

  // Leave evidence that belongs to the first agent, then dispatch the active
  // story again. Dashboard dispatch intentionally opts into story.sh's resume
  // path: the same worktree survives and the replacement agent receives the
  // resume charter instead of a fresh checkout.
  const proofPath = join(worktreePath, "resume-proof.txt");
  writeFileSync(proofPath, "preserve the abandoned agent's work\n");
  await dispatchStory(page);
  const resumedToast = page.locator("#toast-stack .toast.success");
  await expect(resumedToast).toBeVisible({ timeout: DISPATCH_COMPLETION_TIMEOUT });
  await expect(resumedToast).toHaveText(`${ALPHA_STORY_ID} dispatched`);
  expect(readFileSync(proofPath, "utf8")).toBe(
    "preserve the abandoned agent's work\n",
  );
});

test("Auto mode sends agent=claude&auto=1, plus model/effort/speed when selected, and runs a real autonomous dispatch (SH-208, SH-517)", async ({
  page,
}) => {
  test.setTimeout(DISPATCH_COMPLETION_TIMEOUT + 30_000);

  // Seeded directly rather than driven through the token modal -- that flow
  // is AC2's own test; this one is scoped to Auto mode's own behavior.
  await seedToken(page);
  await page.goto("/");

  await openProject(page, "Delta Project");
  await page
    .locator(".card-title", { hasText: "Roll out the new onboarding flow" })
    .click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  // SH-517: a real end-to-end proof that startDispatch's query-string
  // construction reaches the wire correctly -- the endpoint's own contract
  // (tests/dispatch_endpoint.rs) and the modal's own state management
  // (this file's "opens with defaults" test) are each covered on their own,
  // but neither alone proves the two actually agree on the wire.
  const dispatchRequest = page.waitForRequest(
    (req) =>
      req.method() === "POST" &&
      req.url().includes("/dispatch?agent=claude&auto=1&model=haiku&effort=max&speed=fast"),
  );
  await dispatchStory(page, { auto: true, model: "haiku", effort: "max", speed: "fast" });
  await dispatchRequest;

  await expect(page.locator("#dispatch-btn")).toBeDisabled();
  await expect(page.locator("#dispatch-btn")).toHaveText("Dispatching…");

  // SH-232 sent every --auto result to a durable row; SH-304's council
  // narrowed that to the outcomes it protects. A SUCCEEDING autonomous
  // dispatch is corroborated by the story moving on the board and by the
  // tmux window that now exists, so its notice clears itself like any other
  // success -- and says `(auto)`, which is this spec's observation that the
  // daemon forwarded the flag all the way to the script and back.
  const toast = page.locator("#toast-stack .toast.success");
  await expect(toast).toBeVisible({ timeout: DISPATCH_COMPLETION_TIMEOUT });
  await expect(toast).toHaveText(`${DELTA_STORY_ID} dispatched (auto)`);
  // story.sh's own paragraph -- the ~90 words SH-304 was filed about -- no
  // longer reaches the UI at all on the success path.
  await expect(toast).not.toContainText(/utonomous/);
  await expect(page.locator("#dispatch-history .dispatch-history-row")).toHaveCount(0);

  await expect(page.locator("#dispatch-btn")).toBeEnabled();
  await expect(page.locator("#dispatch-btn")).toHaveText("Dispatch");

  const worktreePath = join(
    DELTA_CHECKOUT,
    ".claude/worktrees",
    DELTA_STORY_ID,
  );
  expect(
    existsSync(worktreePath),
    `expected a real worktree at ${worktreePath}`,
  ).toBe(true);

  // And it clears itself, unprompted -- SH-304's second ask, on the exact
  // surface its screenshot showed. The durable half of SH-232 lives on for
  // the outcomes that need it: `notification-contract.spec.ts` drives a
  // refused --auto dispatch (which a real story.sh cannot be asked for on
  // demand) and holds its row to the same 5.5s probe this used to.
  await expect(page.locator("#toast-stack .toast")).toHaveCount(0, {
    timeout: 10_000,
  });
});

test("a saved token is not asked for again on a second dispatch", async ({
  page,
}) => {
  test.setTimeout(DISPATCH_COMPLETION_TIMEOUT + 30_000);

  // Dispatching Alpha's other story reuses a token already in this tab's
  // cookie jar -- Playwright starts each test with a fresh context, so
  // this seeds it directly rather than depending on the previous test
  // having run first.
  await seedToken(page);
  await page.goto("/");

  await openProject(page, "Alpha Project");
  await page
    .locator(".card-title", { hasText: "Fix the flaky upload test" })
    .click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  const dispatchButton = page.locator("#dispatch-btn");
  await dispatchStory(page);

  // No modal this time -- the saved token goes straight onto the request.
  await expect(page.locator("#token-modal")).not.toHaveClass(/open/);
  await expect(dispatchButton).toBeDisabled();

  const toast = page.locator("#toast-stack .toast.success");
  await expect(toast).toBeVisible({ timeout: DISPATCH_COMPLETION_TIMEOUT });
});
