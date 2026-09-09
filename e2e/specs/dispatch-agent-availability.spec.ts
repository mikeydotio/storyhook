import type { Page, Route } from "@playwright/test";
import { test, expect } from "./support";
import { latch, openProject, seedToken } from "./support";

type AgentAvailability = {
  id: "claude" | "codex";
  label: string;
  installed: boolean;
};

const EMPTY_CAPABILITIES = {
  ok: true,
  models: [],
  efforts: [],
  speeds: [],
};

function dispatchOptions(agents: AgentAvailability[]) {
  return {
    agents,
    claude: EMPTY_CAPABILITIES,
    codex: EMPTY_CAPABILITIES,
  };
}

async function fulfillOptions(route: Route, agents: AgentAvailability[]): Promise<void> {
  await route.fulfill({
    status: 200,
    contentType: "application/json",
    body: JSON.stringify(dispatchOptions(agents)),
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
  await openEngineModal(page);
}

async function openEngineModal(page: Page): Promise<void> {
  await expect(page.locator(".engine-run-btn")).toBeEnabled();
  await page.locator(".engine-run-btn").click();
  await expect(page.locator("#engine-modal")).toHaveClass(/open/);
}

test.beforeEach(async ({ page }) => {
  await seedToken(page);
});

test("unavailable providers stay visible and a remembered unavailable provider falls back in both launchers", async ({
  page,
}) => {
  await page.addInitScript(() => {
    localStorage.setItem(
      "storyhook.dispatch.defaults",
      JSON.stringify({ agent: "codex", auto: false }),
    );
  });
  await page.route("**/api/dispatch-options", (route) =>
    fulfillOptions(route, [
      { id: "claude", label: "Claude", installed: true },
      { id: "codex", label: "Codex", installed: false },
    ]),
  );
  await page.goto("/");

  await openDispatchModal(page);
  await expect(page.locator("#dispatch-agent option")).toHaveText([
    "Claude",
    "Codex (not installed)",
  ]);
  await expect(page.locator('#dispatch-agent option[value="claude"]')).toBeEnabled();
  await expect(page.locator('#dispatch-agent option[value="codex"]')).toBeDisabled();
  await expect(page.locator("#dispatch-agent")).toHaveValue("claude");
  await expect(page.locator("#dispatch-modal-submit")).toBeEnabled();
  expect(
    await page.evaluate(() =>
      JSON.parse(localStorage.getItem("storyhook.dispatch.defaults") || "{}").agent,
    ),
  ).toBe("codex");

  await page.locator("#dispatch-modal-cancel").click();
  await closeDrawerAndOpenEngineModal(page);
  await expect(page.locator("#engine-agent option")).toHaveText([
    "Claude",
    "Codex (not installed)",
  ]);
  await expect(page.locator('#engine-agent option[value="codex"]')).toBeDisabled();
  await expect(page.locator("#engine-agent")).toHaveValue("claude");
  await expect(page.locator("#engine-modal-submit")).toBeEnabled();
});

test("no installed provider disables both launchers and their dependent controls", async ({ page }) => {
  const unavailable = [
    { id: "claude", label: "Claude", installed: false },
    { id: "codex", label: "Codex", installed: false },
  ] satisfies AgentAvailability[];
  await page.route("**/api/dispatch-options", (route) => fulfillOptions(route, unavailable));
  await page.goto("/");

  await openDispatchModal(page);
  await expect(page.locator("#dispatch-agent option")).toHaveText([
    "Claude (not installed)",
    "Codex (not installed)",
  ]);
  await expect(page.locator("#dispatch-agent option:enabled")).toHaveCount(0);
  await expect(page.locator("#dispatch-modal-submit")).toBeDisabled();
  await expect(page.locator("#dispatch-model")).toBeDisabled();
  await expect(page.locator("#dispatch-effort")).toBeDisabled();
  await expect(page.locator("#dispatch-speed")).toBeDisabled();

  await page.locator("#dispatch-modal-cancel").click();
  await closeDrawerAndOpenEngineModal(page);
  await expect(page.locator("#engine-agent option:enabled")).toHaveCount(0);
  await expect(page.locator("#engine-modal-submit")).toBeDisabled();
  await expect(page.locator("#engine-model")).toBeDisabled();
  await expect(page.locator("#engine-effort")).toBeDisabled();
  await expect(page.locator("#engine-speed")).toBeDisabled();
});

test("availability loading gates submission and a failed lookup restores the compatible fallback", async ({
  page,
}) => {
  const gate = latch();
  await page.route("**/api/dispatch-options", async (route) => {
    await gate.held;
    await route.abort("failed");
  });
  await page.goto("/");

  await openDispatchModal(page);
  await expect(page.locator("#dispatch-modal-submit")).toBeDisabled();
  gate.release();

  await expect(page.locator("#dispatch-agent option")).toHaveText(["Claude", "Codex"]);
  await expect(page.locator("#dispatch-agent option:disabled")).toHaveCount(0);
  await expect(page.locator("#dispatch-modal-submit")).toBeEnabled();
  await expect(page.locator("#dispatch-model")).toBeEnabled();
  await expect(page.locator("#dispatch-effort")).toBeEnabled();
  await expect(page.locator("#dispatch-speed")).toBeEnabled();
});

test("an installed provider chosen while availability loads remains selected", async ({ page }) => {
  const gate = latch();
  await page.route("**/api/dispatch-options", async (route) => {
    await gate.held;
    await fulfillOptions(route, [
      { id: "claude", label: "Claude", installed: true },
      { id: "codex", label: "Codex", installed: true },
    ]);
  });
  await page.goto("/");

  await openDispatchModal(page);
  await page.locator("#dispatch-agent").selectOption("codex");
  await expect(page.locator("#dispatch-modal-submit")).toBeDisabled();
  gate.release();

  await expect(page.locator("#dispatch-agent")).toHaveValue("codex");
  await expect(page.locator("#dispatch-modal-submit")).toBeEnabled();
});

test("Full Auto lifecycle sync preserves the availability loading gate", async ({ page }) => {
  const gate = latch();
  await page.route("**/api/dispatch-options", async (route) => {
    await gate.held;
    await fulfillOptions(route, [
      { id: "claude", label: "Claude", installed: true },
      { id: "codex", label: "Codex", installed: true },
    ]);
  });
  await page.goto("/");
  await openProject(page, "Alpha Project");

  await openEngineModal(page);
  await expect(page.locator("#engine-agent")).toBeEnabled();
  await expect(page.locator("#engine-model")).toBeDisabled();
  await expect(page.locator("#engine-effort")).toBeDisabled();
  await expect(page.locator("#engine-speed")).toBeDisabled();
  await expect(page.locator("#engine-modal-submit")).toBeDisabled();
  gate.release();

  await expect(page.locator("#engine-model")).toBeEnabled();
  await expect(page.locator("#engine-effort")).toBeEnabled();
  await expect(page.locator("#engine-speed")).toBeEnabled();
  await expect(page.locator("#engine-modal-submit")).toBeEnabled();
});
