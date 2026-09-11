import { test, expect, openProject, seedToken, projectSlug } from "./support";

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

test("verifier menu hosts non-editable commands and supports keyboard navigation", async ({ page }) => {
  const stop = page.locator('.column[data-state="verifying"]')
    .getByRole("button", { name: "Stop verifier", exact: true });
  await stop.focus();
  await stop.press("Enter");
  const menu = page.getByRole("menu", { name: "Stop verifier" });
  const items = menu.getByRole("menuitem");
  await expect(items).toHaveCount(2);
  // This is the no-typing invariant behind the direct keydown receiver entry.
  await expect(menu.locator('input, textarea, select, [contenteditable]:not([contenteditable="false"])')).toHaveCount(0);
  for (const item of await items.all()) {
    await expect(item).toHaveJSProperty("isContentEditable", false);
  }
  await expect(items.nth(0)).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(items.nth(1)).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(items.nth(0)).toBeFocused();
  await page.keyboard.press("ArrowUp");
  await expect(items.nth(1)).toBeFocused();
  await page.keyboard.press("Home");
  await expect(items.nth(0)).toBeFocused();
  await page.keyboard.press("End");
  await expect(items.nth(1)).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(menu).toHaveCount(0);
  await expect(stop).toBeFocused();
});

test("verifier stop menu drains, persists after reload, and starts again", async ({ page }) => {
  const column = page.locator('.column[data-state="verifying"]');
  const stop = column.getByRole("button", { name: "Stop verifier", exact: true });
  await stop.click();
  const menu = page.getByRole("menu", { name: "Stop verifier" });
  await expect(menu.getByRole("menuitem", { name: "Let inflight verifications finish" })).toBeVisible();
  await expect(menu.getByRole("menuitem", { name: "Stop inflight verifications" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(stop).toBeFocused();
  await stop.press("Enter");
  await page.getByRole("menuitem", { name: "Let inflight verifications finish" }).press("Enter");
  const start = column.getByRole("button", { name: "Start verifier", exact: true });
  await expect(start).toBeVisible();
  await expect(start.locator('[data-emoji="play"]')).toBeVisible();
  await page.reload();
  await expect(start).toBeVisible();
  await start.click();
  await expect(stop).toBeVisible();
  await expect(stop.locator('[data-emoji="stop"]')).toBeVisible();
});

test("stop inflight choice leaves an idle verifier stopped until explicitly started", async ({ page }) => {
  const column = page.locator('.column[data-state="verifying"]');
  await column.getByRole("button", { name: "Stop verifier", exact: true }).click();
  await page.getByRole("menuitem", { name: "Stop inflight verifications" }).click();
  const start = column.getByRole("button", { name: "Start verifier", exact: true });
  await expect(start).toBeVisible();
  await start.click();
  await expect(column.getByRole("button", { name: "Stop verifier", exact: true })).toBeVisible();
});

test("a rejected stop keeps the running control enabled", async ({ page, request }) => {
  const slug = await projectSlug(request, "Alpha Project");
  const path = `/api/repos/${encodeURIComponent(slug)}/verification/control`;
  await page.route((url) => url.pathname === path, (route) => route.fulfill({
    status: 422, json: { error: "Verifier control fixture refusal" },
  }));
  const column = page.locator('.column[data-state="verifying"]');
  const stop = column.getByRole("button", { name: "Stop verifier", exact: true });
  await stop.click();
  await page.getByRole("menuitem", { name: "Stop inflight verifications" }).click();
  await expect(page.getByText("Verifier control fixture refusal", { exact: false })).toBeVisible();
  await expect(stop).toBeEnabled();
  await expect(column.getByRole("button", { name: "Start verifier", exact: true })).toHaveCount(0);
});

test("draining can escalate and restart waits for owned cleanup", async ({ page, request }) => {
  const slug = await projectSlug(request, "Alpha Project");
  const base = `/api/repos/${encodeURIComponent(slug)}`;
  let mode = "draining";
  await page.route((url) => url.pathname === `${base}/data`, async (route) => {
    const response = await route.fetch();
    const data = await response.json();
    data.verification_control = { state: mode };
    await route.fulfill({ response, json: data });
  });
  await page.route((url) => url.pathname === `${base}/verification/control`, async (route) => {
    expect(route.request().postDataJSON()).toEqual({ action: "stop" });
    mode = "stopping";
    await route.fulfill({ status: 200, json: { state: mode } });
  });
  await page.reload();
  const column = page.locator('.column[data-state="verifying"]');
  await expect(column.getByRole("status")).toHaveText("Finishing inflight verification…");
  await column.getByRole("button", { name: "Stop verifier", exact: true }).click();
  const menu = page.getByRole("menu", { name: "Stop verifier" });
  await expect(menu.getByRole("menuitem", { name: "Let inflight verifications finish" })).toHaveAttribute("aria-disabled", "true");
  const cancel = menu.getByRole("menuitem", { name: "Stop inflight verifications" });
  await expect(cancel).toBeFocused();
  await cancel.press("Enter");
  await expect(column.getByRole("button", { name: "Stopping verifier…", exact: true })).toBeDisabled();
  await expect(column.getByRole("status")).toHaveText("Stopping inflight verification…");
  mode = "stopped";
  await page.reload();
  await expect(column.getByRole("button", { name: "Start verifier", exact: true })).toBeEnabled();
});
