import { test, expect } from "./support";
import { withDrainedRoutes } from "../route-lifetime";
import {
  awaitNoOverlay,
  cleanUpCreatedStories,
  createStory,
  deleteStory,
  openProject,
  seedToken,
} from "./support";

/**
 * SH-204: create and drawer label entry are one interaction model. Both
 * canonicalize case and commit the current token on comma, Enter, or focus
 * leaving the field; the drawer persists each committed change immediately.
 */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

test("create and drawer label comboboxes commit comma and Tab as lowercase chips", async ({
  page,
}) => {
  const title = `SH-204 label editor ${Date.now()}`;
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);

  const createInput = page.locator("#create-labels-field .label-combobox input");
  await createInput.fill("Web");
  await createInput.press(",");
  await expect(page.locator("#create-labels-field .label-chip", { hasText: "web" })).toBeVisible();
  await createInput.fill("API");
  await createInput.press("Tab");
  await expect(page.locator("#create-labels-field .label-chip", { hasText: "api" })).toBeVisible();

  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  const card = page.locator('.column[data-state="todo"] .card', { hasText: title });
  await expect(card).toBeVisible();
  await awaitNoOverlay(page);
  await card.getByText(title, { exact: true }).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  const drawerInput = page.locator('#drawer .label-combobox input[data-field="label-add"]');
  await drawerInput.fill("Plugin");
  await drawerInput.press(",");
  await expect(page.locator("#drawer .label-chip", { hasText: "plugin" })).toBeVisible();
  await drawerInput.fill("CLI");
  await drawerInput.press("Tab");
  await expect(page.locator("#drawer .label-chip", { hasText: "cli" })).toBeVisible();

  const plugin = page.locator("#drawer .label-chip", { hasText: "plugin" });
  await plugin.locator("button").click();
  await expect(plugin).toHaveCount(0);

  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  const suggestionTitle = `SH-204 label suggestion ${Date.now()}`;
  await createStory(page, suggestionTitle);
  await page
    .locator('.column[data-state="todo"] .card', { hasText: suggestionTitle })
    .click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  const suggestionInput = page.locator('#drawer input[data-field="label-add"]');
  await suggestionInput.fill("ap");
  const apiSuggestion = page.locator("#drawer .fdd-option", { hasText: "api" });
  await expect(apiSuggestion).toBeVisible();
  await apiSuggestion.click();
  await expect(page.locator("#drawer .label-chip", { hasText: "api" })).toBeVisible();

  await page.locator("#drawer-close").click();
  await deleteStory(page, suggestionTitle);
  await deleteStory(page, title);
});

test("a failed drawer label write restores the token for an explicit retry", async ({
  page,
}) => {
  const title = `SH-204 label retry ${Date.now()}`;
  await createStory(page, title);
  await page.locator('.column[data-state="todo"] .card', { hasText: title }).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  const labelsEndpoint = /\/api\/repos\/[^/]+\/story\/[^/]+\/labels$/;
  let refused = false;
  await page.route(labelsEndpoint, async (route) => {
    if (refused) {
      await route.continue();
      return;
    }
    refused = true;
    await route.fulfill({ status: 500, json: { error: "simulated label refusal" } });
  });

  const input = page.locator('#drawer input[data-field="label-add"]');
  await input.fill("RetryLabel");
  await input.press("Enter");
  await expect(page.locator("#drawer .label-chip", { hasText: "retrylabel" })).toHaveCount(0);
  await expect(input).toHaveValue("RetryLabel");
  await expect(page.locator("#toast-stack .toast.error")).toContainText(
    "simulated label refusal",
  );

  await input.press("Enter");
  await expect(page.locator("#drawer .label-chip", { hasText: "retrylabel" })).toBeVisible();

  await page.locator("#drawer-close").click();
  await deleteStory(page, title);
});

for (const operation of ["add", "remove"] as const) {
  for (const outcome of ["success", "failure"] as const) {
    test(`a pending label ${operation} exposes its busy state until ${outcome}`, async ({ page }) => {
      const title = `SH-812 pending label ${operation} ${outcome}`;
      await createStory(page, title);
      const card = page.locator('.column[data-state="todo"] .card', { hasText: title });
      await card.click();
      const input = page.locator('#drawer input[data-field="label-add"]');
      const combobox = page.locator("#drawer .label-combobox");
      const chips = combobox.locator(".label-chip");
      const endpoint = /\/api\/repos\/[^/]+\/story\/[^/]+\/labels$/;
      const seeded = page.waitForResponse((response) => endpoint.test(response.url()));
      await input.fill("alpha,beta");
      await input.press("Enter");
      await (await seeded).finished();
      // Reopen the confirmed record so this proof starts with an idle editor.
      await page.locator("#drawer-close").click();
      await card.click();
      await expect(chips).toHaveCount(2);

      let release!: () => void;
      const held = new Promise<void>((resolve) => { release = resolve; });
      let markTaken!: () => void;
      const taken = new Promise<void>((resolve) => { markTaken = resolve; });
      let first = true;
      await withDrainedRoutes(page, async () => {
        await page.route(endpoint, async (route) => {
          if (!first) {
            await route.continue();
            return;
          }
          first = false;
          markTaken();
          // Hold before the daemon writes, so no SSE response can confirm it.
          await held;
          if (outcome === "failure")
            await route.fulfill({ status: 500, json: { error: "held label refusal" } });
          else
            await route.continue();
        });
        try {
          await input.fill(operation === "add" ? "Gamma" : "draft");
          if (operation === "add") await input.press("Enter");
          else await chips.filter({ hasText: "alpha" }).getByRole("button").click();
          await taken;
          await expect(input).toHaveJSProperty("readOnly", true);
          await expect(input).toBeFocused();
          await expect(combobox).toHaveAttribute("aria-busy", "true");
          await expect(combobox.getByRole("status")).toHaveText("Saving labels…");
          for (const button of await chips.getByRole("button").all())
            await expect(button).toBeDisabled();
          await input.pressSequentially("lost");
          await expect(input).toHaveValue(operation === "add" ? "" : "draft");
        } finally {
          release();
        }
        await expect(input).toBeEditable();
        await expect(combobox).toHaveAttribute("aria-busy", "false");
        await expect(combobox.getByRole("status")).toBeHidden();
        const expected = outcome === "failure" ? 2 : operation === "add" ? 3 : 1;
        await expect(chips).toHaveCount(expected);
        for (const button of await chips.getByRole("button").all())
          await expect(button).toBeEnabled();
        if (outcome === "failure") {
          await expect(page.locator("#toast-stack .toast.error")).toContainText("held label refusal");
          await expect(input).toHaveValue(operation === "add" ? "Gamma" : "draft");
        }
        await input.fill("next");
        await input.press("Enter");
        await expect(chips.filter({ hasText: "next" })).toBeVisible();
        await expect(input).toBeEditable();
        await page.locator("#drawer-close").click();
        await card.click();
        await expect(chips.filter({ hasText: "next" })).toBeVisible();
      });
    });
  }
}
