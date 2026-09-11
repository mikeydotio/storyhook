import { test, expect, cleanUpCreatedStories, openProject, seedToken, createStory } from "./support";

cleanUpCreatedStories("Alpha Project");
test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

test("Reset offers explicit Force, cancels safely, and returns a story to Todo", async ({ page }) => {
  const title = "SH-664 reset through card menu";
  const id = await createStory(page, title);
  const card = page.locator('.card[data-id="' + id + '"]');
  await card.click({ button: "right" });
  await page.getByRole("menuitem", { name: "Set Status", exact: true }).click();
  await page.locator(".ctxmenu-sub").getByRole("menuitem", { name: "in-progress", exact: true }).click();
  await expect(page.locator('.column[data-state="in-progress"] .card[data-id="' + id + '"]')).toBeVisible();
  await card.click({ button: "right" });
  await page.getByRole("menuitem", { name: "Reset", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Reset story", exact: true });
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("checkbox", { name: "Force", exact: true })).not.toBeChecked();
  await dialog.getByRole("checkbox", { name: "Force", exact: true }).check();
  await page.keyboard.press("Escape");
  await expect(dialog).not.toBeVisible();
  await expect(card).toBeVisible();
  await card.click({ button: "right" });
  await page.getByRole("menuitem", { name: "Reset", exact: true }).click();
  await expect(dialog.getByRole("checkbox", { name: "Force", exact: true })).not.toBeChecked();
  const request = page.waitForRequest(request => request.method() === "POST" && request.url().endsWith("/story/" + id + "/reset"));
  await dialog.getByRole("button", { name: "Reset story", exact: true }).click();
  expect((await request).postDataJSON()).toEqual({ force: false });
  await expect(dialog).not.toBeVisible();
  await expect(page.locator('.column[data-state="todo"] .card[data-id="' + id + '"]')).toBeVisible();
});

test("Force is sent only after opt-in and a concurrent closure stays an actionable error", async ({ page }) => {
  const id = await createStory(page, "SH-664 force and refusal");
  const card = page.locator('.card[data-id="' + id + '"]');
  await card.click({ button: "right" });
  await page.getByRole("menuitem", { name: "Reset", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Reset story", exact: true });
  await dialog.getByRole("checkbox", { name: "Force", exact: true }).check();
  // A real second client closes the story after this dialog's initial snapshot.
  const { projectSlug } = await import("./support");
  const slug = await projectSlug(page.request, "Alpha Project");
  const closed = await page.request.post(`/api/repos/${slug}/story/${id}/move`, {
    headers: { "X-Storyhook": "1" }, data: { state: "done" },
  });
  expect(closed.ok()).toBeTruthy();
  const request = page.waitForRequest(request => request.method() === "POST" && request.url().endsWith("/story/" + id + "/reset"));
  await dialog.getByRole("button", { name: "Reset story", exact: true }).click();
  expect((await request).postDataJSON()).toEqual({ force: true });
  await expect(dialog.getByRole("alert")).toContainText("closed");
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Cancel", exact: true })).toBeEnabled();
  await dialog.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(dialog).not.toBeVisible();
});
