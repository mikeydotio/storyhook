import { test, expect } from "./support";
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
