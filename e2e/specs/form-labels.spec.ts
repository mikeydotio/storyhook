import { expect, test } from "./support";
import type { Locator } from "@playwright/test";
import { openProject, seedToken } from "./support";

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await expect(
    page.locator(".repo-card-name", { hasText: "Alpha Project" }),
  ).toBeVisible();
});

/**
 * Returns every text-entry control whose placeholder is its only candidate
 * label. Placeholder text is intentionally excluded: it disappears after
 * entry and is not a durable programmatic association (SH-328).
 */
async function placeholderOnlyControls(root: Locator): Promise<string[]> {
  return root
    .locator("input[placeholder], textarea[placeholder]")
    .evaluateAll((controls) =>
      controls
        .filter((control) => {
          const field = control as HTMLInputElement | HTMLTextAreaElement;
          return (
            !field.labels?.length &&
            !field.getAttribute("aria-label")?.trim() &&
            !field.getAttribute("aria-labelledby")?.trim()
          );
        })
        .map((control) => {
          const field = control as HTMLInputElement | HTMLTextAreaElement;
          return field.id
            ? `#${field.id}`
            : field.getAttribute("placeholder") || field.tagName;
        }),
    );
}

test("project registration keeps visible, associated labels after entry", async ({ page }) => {
  await page.locator("#settings-btn").click();

  const settings = page.locator("#settings-view");
  const fields = [
    { name: "Checkout path on this machine", value: "/tmp/storyhook-label-test" },
    { name: "Display name (optional)", value: "Label test" },
    { name: "Story ID prefix (required, for example SH)", value: "LT" },
  ];

  for (const field of fields) {
    const input = settings.getByLabel(field.name, { exact: true });
    await expect(input).toBeVisible();
    await expect(input).toHaveAccessibleName(field.name);
    await input.fill(field.value);
    await expect(input).toHaveValue(field.value);
    await expect(
      settings.locator(`label[for="${await input.getAttribute("id")}"]`),
    ).toBeVisible();
  }
});

test("no dashboard text-entry control relies on a placeholder as its label", async ({ page }) => {
  // Static modal controls and the compact search field are always in the DOM.
  expect(await placeholderOnlyControls(page.locator("body"))).toEqual([]);

  await page.locator("#settings-btn").click();
  const settings = page.locator("#settings-view");
  await expect(settings).toBeVisible();
  expect(await placeholderOnlyControls(settings)).toEqual([]);

  await settings
    .locator(".settings-table tbody tr", { hasText: "Alpha Project" })
    .getByRole("button", { name: "Delete" })
    .click();
  await expect(page.locator("#settings-delete input")).toBeVisible();
  expect(await placeholderOnlyControls(page.locator("#settings-delete"))).toEqual(
    [],
  );
  await page
    .locator("#settings-delete")
    .getByRole("button", { name: "Cancel" })
    .click();

  await page.locator("#home-btn").click();
  await openProject(page, "Alpha Project");
  await page.locator(".card").first().click();
  const drawer = page.locator("#drawer");
  await expect(drawer).toHaveClass(/open/);
  expect(await placeholderOnlyControls(drawer)).toEqual([]);
});
