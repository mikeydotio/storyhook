import { test, expect, openProject, seedToken, projectSlug } from "./support";

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
  const toggle = page.getByRole("checkbox", { name: "enable automations", exact: true });
  await toggle.check();
  await expect(toggle).toBeEnabled();
});

test("project automation toggle persists and restores enabled controls", async ({ page }) => {
  const toggle = page.getByRole("checkbox", { name: "enable automations", exact: true });
  await expect(toggle).toBeChecked();
  await toggle.uncheck();
  await expect(toggle).toBeEnabled();
  await expect(toggle).not.toBeChecked();
  await expect(page.locator('#verification-banner-region')).toBeEmpty();
  await expect(page.getByRole("button", { name: "Stop verifier", exact: true })).toBeHidden();
  await page.reload();
  await expect(toggle).not.toBeChecked();
  await toggle.check();
  await expect(toggle).toBeEnabled();
  await expect(page.getByRole("button", { name: "Stop verifier", exact: true })).toBeVisible();
});

test("a rejected toggle keeps the confirmed state and permits retry", async ({ page, request }) => {
  const slug = await projectSlug(request, "Alpha Project");
  const path = `/api/repos/${encodeURIComponent(slug)}/automations`;
  await page.route((url) => url.pathname === path, route => route.fulfill({
    status: 422, json: { error: "Manual-mode fixture refusal" },
  }));
  const toggle = page.getByRole("checkbox", { name: "enable automations", exact: true });
  await toggle.uncheck();
  await expect(page.getByText("Manual-mode fixture refusal", { exact: false })).toBeVisible();
  await expect(toggle).toBeEnabled();
  await expect(toggle).toBeChecked();
});

test("pending toggle excludes duplicate requests", async ({ page, request }) => {
  const slug = await projectSlug(request, "Alpha Project");
  const path = `/api/repos/${encodeURIComponent(slug)}/automations`;
  let release!: () => void;
  const pending = new Promise<void>(resolve => { release = resolve; });
  let calls = 0;
  await page.route((url) => url.pathname === path, async route => {
    calls++;
    await pending;
    await route.fulfill({ status: 422, json: { error: "Retry fixture" } });
  });
  const toggle = page.getByRole("checkbox", { name: "enable automations", exact: true });
  await toggle.uncheck();
  await expect(toggle).toBeDisabled();
  expect(calls).toBe(1);
  release();
  await expect(toggle).toBeEnabled();
  await expect(toggle).toBeChecked();
});
