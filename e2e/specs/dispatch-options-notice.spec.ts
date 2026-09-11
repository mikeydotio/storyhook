import type { Page, Route } from "@playwright/test";
import { test, expect } from "./support";
import { openProject, seedToken } from "./support";

/**
 * SH-670: a provider slot the daemon degraded to `{ok:false, reason}` used
 * to render exactly like a provider with no options -- Model, Effort and
 * Speed holding a bare "Default" -- and the `reason` it carried, the one
 * line naming the fix, was thrown away. The live payload that produced the
 * story is the first fixture below: Claude's helper unresolvable, Codex's
 * catalog intact. A failed catalog fetch is the same silence one layer up.
 *
 * Neither case gates Submit. SH-517 degrades the selector rather than
 * dispatch, and SH-616's availability row (`agents[].installed`) owns
 * whether a provider is launchable; `dispatch-agent-availability.spec.ts`
 * covers that row and is deliberately not repeated here.
 */

const RESOLVER_REASON =
  "could not find plugins/story/bin/story.sh for agent `claude` -- install it with " +
  "`story plugin install claude` or set STORYHOOK_DISPATCH_SCRIPT";

const TAIL = " Model, Effort and Speed will use the provider's defaults.";

const BOTH_INSTALLED = [
  { id: "claude", label: "Claude", installed: true },
  { id: "codex", label: "Codex", installed: true },
];

const CLAUDE_CATALOG = {
  ok: true,
  agent: "claude",
  models: [
    { id: "opusplan", label: "Opus+Sonnet", default: true },
    { id: "opus", label: "Opus" },
  ],
  efforts: [{ id: "low" }, { id: "high" }],
  speeds: [{ id: "fast", label: "Fast" }],
};

const CODEX_CATALOG = {
  ok: true,
  agent: "codex",
  models: [
    { id: "gpt-6-astra", label: "GPT-6 Astra" },
    { id: "gpt-5.6-sol", label: "GPT-5.6 Sol" },
  ],
  efforts: [{ id: "none" }, { id: "low" }, { id: "ultra" }],
  speeds: [{ id: "fast", label: "Fast" }],
};

const CLAUDE_UNRESOLVABLE = { ok: false, agent: "claude", reason: RESOLVER_REASON };

async function fulfill(route: Route, body: unknown): Promise<void> {
  await route.fulfill({
    status: 200,
    contentType: "application/json",
    body: JSON.stringify(body),
  });
}

async function openDispatchModal(page: Page): Promise<void> {
  await openProject(page, "Alpha Project");
  await page.locator(".card-title", { hasText: "Wire up the auth flow" }).click();
  await page.locator("#dispatch-btn").click();
  await expect(page.locator("#dispatch-modal")).toHaveClass(/open/);
}

async function closeDrawerAndOpenEngineModal(page: Page): Promise<void> {
  await page.locator("#drawer-close").click();
  await expect(page.locator(".engine-run-btn")).toBeEnabled();
  await page.locator(".engine-run-btn").click();
  await expect(page.locator("#engine-modal")).toHaveClass(/open/);
}

/** The three provider-scoped selects of one launcher, by prefix. */
function selectors(page: Page, prefix: "dispatch" | "engine") {
  return {
    model: page.locator(`#${prefix}-model option`),
    effort: page.locator(`#${prefix}-effort option`),
    speed: page.locator(`#${prefix}-speed option`),
    notice: page.locator(`#${prefix}-options-notice`),
    agent: page.locator(`#${prefix}-agent`),
    submit: page.locator(`#${prefix}-modal-submit`),
  };
}

test.beforeEach(async ({ page }) => {
  await seedToken(page);
});

test("a provider whose helper the daemon could not resolve says so, in both launchers, and Submit stays enabled", async ({
  page,
}) => {
  await page.route("**/api/dispatch-options", (route) =>
    fulfill(route, { agents: BOTH_INSTALLED, claude: CLAUDE_UNRESOLVABLE, codex: CODEX_CATALOG }),
  );
  await page.goto("/");

  await openDispatchModal(page);
  const dispatch = selectors(page, "dispatch");
  await expect(dispatch.agent).toHaveValue("claude");
  await expect(dispatch.notice).toHaveText("Claude options unavailable: " + RESOLVER_REASON + "." + TAIL);
  await expect(dispatch.model).toHaveText(["Default"]);
  await expect(dispatch.effort).toHaveText(["Default"]);
  await expect(dispatch.speed).toHaveText(["Default"]);
  await expect(dispatch.submit).toBeEnabled();
  await expect(page.locator("#dispatch-model")).toBeEnabled();
  await expect(page.locator("#dispatch-effort")).toBeEnabled();

  // The notice belongs to the provider showing, not to the dialog: Codex's
  // catalog is intact, so switching to it clears the sentence and fills the
  // selects; switching back brings the sentence back.
  await dispatch.agent.selectOption("codex");
  await expect(dispatch.notice).toHaveText("");
  await expect(dispatch.model).toHaveText(["Default", "GPT-6 Astra", "GPT-5.6 Sol"]);
  await expect(dispatch.effort).toHaveText(["Default", "none", "low", "ultra"]);
  await dispatch.agent.selectOption("claude");
  await expect(dispatch.notice).toHaveText("Claude options unavailable: " + RESOLVER_REASON + "." + TAIL);
  await expect(dispatch.model).toHaveText(["Default"]);

  // Cancel clears it, so a later open never shows a stale sentence before
  // the catalog has been (re)applied.
  await page.locator("#dispatch-modal-cancel").click();
  await expect(dispatch.notice).toHaveText("");

  await closeDrawerAndOpenEngineModal(page);
  const engine = selectors(page, "engine");
  await expect(engine.agent).toHaveValue("claude");
  await expect(engine.notice).toHaveText("Claude options unavailable: " + RESOLVER_REASON + "." + TAIL);
  await expect(engine.model).toHaveText(["Default"]);
  await expect(engine.effort).toHaveText(["Default"]);
  await expect(engine.submit).toBeEnabled();
  await engine.agent.selectOption("codex");
  await expect(engine.notice).toHaveText("");
  await expect(engine.model).toHaveText(["Default", "GPT-6 Astra", "GPT-5.6 Sol"]);
  // A catalog note and a submit error are different facts on different
  // surfaces; opening never wrote one over the other.
  await expect(page.locator("#engine-modal-error")).toHaveText("");
  await page.locator("#engine-modal-cancel").click();
  await expect(engine.notice).toHaveText("");
});

test("a healthy catalog shows no notice, before or after a provider switch", async ({ page }) => {
  await page.route("**/api/dispatch-options", (route) =>
    fulfill(route, { agents: BOTH_INSTALLED, claude: CLAUDE_CATALOG, codex: CODEX_CATALOG }),
  );
  await page.goto("/");

  await openDispatchModal(page);
  const dispatch = selectors(page, "dispatch");
  await expect(dispatch.model).toHaveText(["Default", "Opus+Sonnet", "Opus"]);
  await expect(dispatch.effort).toHaveText(["Default", "low", "high"]);
  await expect(dispatch.notice).toHaveText("");
  await dispatch.agent.selectOption("codex");
  await expect(dispatch.model).toHaveText(["Default", "GPT-6 Astra", "GPT-5.6 Sol"]);
  await expect(dispatch.notice).toHaveText("");
  await dispatch.agent.selectOption("claude");
  await expect(dispatch.notice).toHaveText("");
  await page.locator("#dispatch-modal-cancel").click();

  await closeDrawerAndOpenEngineModal(page);
  const engine = selectors(page, "engine");
  await expect(engine.model).toHaveText(["Default", "Opus+Sonnet", "Opus"]);
  await expect(engine.notice).toHaveText("");
  await engine.agent.selectOption("codex");
  await expect(engine.notice).toHaveText("");
});

test("a degraded slot with no reason string still explains itself rather than printing undefined", async ({
  page,
}) => {
  await page.route("**/api/dispatch-options", (route) =>
    fulfill(route, {
      agents: BOTH_INSTALLED,
      claude: { ok: false, agent: "claude" },
      codex: { ok: false, agent: "codex", reason: 42 },
    }),
  );
  await page.goto("/");

  await openDispatchModal(page);
  const dispatch = selectors(page, "dispatch");
  await expect(dispatch.notice).toHaveText(
    "Claude options unavailable: the helper reported no catalog." + TAIL,
  );
  await dispatch.agent.selectOption("codex");
  await expect(dispatch.notice).toHaveText(
    "Codex options unavailable: the helper reported no catalog." + TAIL,
  );
  await expect(dispatch.submit).toBeEnabled();
});

test("a failed catalog fetch is named, keeps the compatible fallback, and clears once a later fetch succeeds", async ({
  page,
}) => {
  let fail = true;
  await page.route("**/api/dispatch-options", async (route) => {
    if (fail) {
      await route.fulfill({ status: 500, contentType: "text/plain", body: "helper exploded" });
      return;
    }
    await fulfill(route, { agents: BOTH_INSTALLED, claude: CLAUDE_CATALOG, codex: CODEX_CATALOG });
  });
  await page.goto("/");

  await openDispatchModal(page);
  const dispatch = selectors(page, "dispatch");
  // SH-616's contract for a failed lookup is unchanged: availability is
  // unknown, not negative, so both providers stay enabled and so does
  // Submit. What is new is that the reader is told why the selects are
  // bare.
  await expect(dispatch.agent.locator("option:disabled")).toHaveCount(0);
  await expect(dispatch.submit).toBeEnabled();
  await expect(dispatch.model).toHaveText(["Default"]);
  await expect(dispatch.notice).toContainText("Provider options could not be loaded: ");
  await expect(dispatch.notice).toContainText(TAIL);
  // The sentence survives a provider switch: the failure was the fetch, not
  // one provider's slot.
  await dispatch.agent.selectOption("codex");
  await expect(dispatch.notice).toContainText("Provider options could not be loaded: ");
  await page.locator("#dispatch-modal-cancel").click();
  await expect(dispatch.notice).toHaveText("");

  // A failure is never cached (SH-517), so the next open refetches, and a
  // good answer clears the sentence.
  fail = false;
  await page.locator("#dispatch-btn").click();
  await expect(page.locator("#dispatch-modal")).toHaveClass(/open/);
  await expect(dispatch.model).toHaveText(["Default", "Opus+Sonnet", "Opus"]);
  await expect(dispatch.notice).toHaveText("");
});
